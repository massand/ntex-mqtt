use ntex::util::{ByteString, Bytes, Either};
use std::{fmt, future::Future, num::NonZeroU16, rc::Rc};

use super::shared::{Ack, AckType, MqttShared};
use super::{codec, error::ProtocolError, error::SendPacketError};

pub struct MqttSink(Rc<MqttShared>);

impl Clone for MqttSink {
    fn clone(&self) -> Self {
        MqttSink(self.0.clone())
    }
}

impl MqttSink {
    pub(crate) fn new(state: Rc<MqttShared>) -> Self {
        MqttSink(state)
    }

    /// Get client receive credit
    pub fn credit(&self) -> usize {
        self.0.cap.get() - self.0.queues.borrow().inflight.len()
    }

    /// Get notification when packet could be send to the peer.
    ///
    /// Result indicates if connection is alive
    pub fn ready(&self) -> impl Future<Output = bool> {
        let mut queues = self.0.queues.borrow_mut();
        let res = if !self.0.state.is_open() {
            false
        } else if queues.inflight.len() >= self.0.cap.get() {
            let (tx, rx) = self.0.pool.waiters.channel();
            queues.waiters.push_back(tx);
            return Either::Right(async move { rx.await.is_ok() });
        } else {
            true
        };
        Either::Left(async move { res })
    }

    /// Close mqtt connection
    pub fn close(&self) {
        if self.0.state.is_open() {
            let _ = self.0.state.close();
        }
        let mut queues = self.0.queues.borrow_mut();
        queues.inflight.clear();
        queues.waiters.clear();
    }

    /// Force close mqtt connection. mqtt dispatcher does not wait for uncompleted
    /// responses, but it flushes buffers.
    pub fn force_close(&self) {
        if self.0.state.is_open() {
            let _ = self.0.state.force_close();
        }
        let mut queues = self.0.queues.borrow_mut();
        queues.inflight.clear();
        queues.waiters.clear();
    }

    /// Send ping
    pub(super) fn ping(&self) -> bool {
        self.0.state.write().encode(codec::Packet::PingRequest, &self.0.codec).is_ok()
    }

    /// Create publish message builder
    pub fn publish(&self, topic: ByteString, payload: Bytes) -> PublishBuilder {
        PublishBuilder {
            packet: codec::Publish {
                topic,
                payload,
                dup: false,
                retain: false,
                qos: codec::QoS::AtMostOnce,
                packet_id: None,
            },
            shared: self.0.clone(),
        }
    }

    /// Create subscribe packet builder
    ///
    /// panics if id is 0
    pub fn subscribe(&self) -> SubscribeBuilder {
        SubscribeBuilder { id: 0, topic_filters: Vec::new(), shared: self.0.clone() }
    }

    /// Create unsubscribe packet builder
    pub fn unsubscribe(&self) -> UnsubscribeBuilder {
        UnsubscribeBuilder { id: 0, topic_filters: Vec::new(), shared: self.0.clone() }
    }

    pub(super) fn pkt_ack(&self, pkt: Ack) -> Result<(), ProtocolError> {
        let mut queues = self.0.queues.borrow_mut();

        // check ack order
        if let Some(idx) = queues.inflight_order.pop_front() {
            if idx != pkt.packet_id() {
                log::trace!(
                    "MQTT protocol error, packet_id order does not match, expected {}, got: {}",
                    idx,
                    pkt.packet_id()
                );
            } else {
                // get publish ack channel
                log::trace!("Ack packet with id: {}", pkt.packet_id());
                let idx = pkt.packet_id();
                if let Some((tx, tp)) = queues.inflight.remove(&idx) {
                    if !pkt.is_match(tp) {
                        log::trace!("MQTT protocol error, unexpeted packet");
                        self.close();
                        return Err(ProtocolError::Unexpected(pkt.packet_type(), tp.name()));
                    }
                    let _ = tx.send(pkt);

                    // wake up queued request (receive max limit)
                    while let Some(tx) = queues.waiters.pop_front() {
                        if tx.send(()).is_ok() {
                            break;
                        }
                    }
                    return Ok(());
                } else {
                    log::error!("Inflight state inconsistency")
                }
            }
        } else {
            log::trace!("Unexpected PublishAck packet: {:?}", pkt.packet_id());
        }
        self.close();
        Err(ProtocolError::PacketIdMismatch)
    }
}

impl fmt::Debug for MqttSink {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("MqttSink").finish()
    }
}

pub struct PublishBuilder {
    packet: codec::Publish,
    shared: Rc<MqttShared>,
}

impl PublishBuilder {
    /// Set packet id.
    ///
    /// Note: if packet id is not set, it gets generated automatically.
    /// Packet id management should not be mixed, it should be auto-generated
    /// or set by user. Otherwise collisions could occure.
    ///
    /// panics if id is 0
    pub fn packet_id(mut self, id: u16) -> Self {
        let id = NonZeroU16::new(id).expect("id 0 is not allowed");
        self.packet.packet_id = Some(id);
        self
    }

    /// this might be re-delivery of an earlier attempt to send the Packet.
    pub fn dup(mut self, val: bool) -> Self {
        self.packet.dup = val;
        self
    }

    pub fn retain(mut self) -> Self {
        self.packet.retain = true;
        self
    }

    /// Send publish packet with QoS 0
    pub fn send_at_most_once(self) -> Result<(), SendPacketError> {
        let packet = self.packet;

        if self.shared.state.is_open() {
            log::trace!("Publish (QoS-0) to {:?}", packet.topic);
            self.shared
                .state
                .write()
                .encode(codec::Packet::Publish(packet), &self.shared.codec)
                .map_err(SendPacketError::Encode)
                .map(|_| ())
        } else {
            log::error!("Mqtt sink is disconnected");
            Err(SendPacketError::Disconnected)
        }
    }

    #[allow(clippy::await_holding_refcell_ref)]
    /// Send publish packet with QoS 1
    pub async fn send_at_least_once(self) -> Result<(), SendPacketError> {
        let shared = self.shared;
        let mut packet = self.packet;
        packet.qos = codec::QoS::AtLeastOnce;

        if shared.state.is_open() {
            // handle client receive maximum
            if !shared.has_credit() {
                let (tx, rx) = shared.pool.waiters.channel();
                shared.queues.borrow_mut().waiters.push_back(tx);

                if rx.await.is_err() {
                    return Err(SendPacketError::Disconnected);
                }
            }
            let mut queues = shared.queues.borrow_mut();

            // publish ack channel
            let (tx, rx) = shared.pool.queue.channel();

            // packet id
            let mut idx = packet.packet_id.map(|i| i.get()).unwrap_or(0);
            if idx == 0 {
                idx = shared.next_id();
                packet.packet_id = NonZeroU16::new(idx);
            }
            if queues.inflight.contains_key(&idx) {
                return Err(SendPacketError::PacketIdInUse(idx));
            }
            queues.inflight.insert(idx, (tx, AckType::Publish));
            queues.inflight_order.push_back(idx);

            log::trace!("Publish (QoS1) to {:#?}", packet);

            match shared.state.write().encode(codec::Packet::Publish(packet), &shared.codec) {
                Ok(_) => {
                    // do not borrow cross yield points
                    drop(queues);

                    rx.await.map(|_| ()).map_err(|_| SendPacketError::Disconnected)
                }
                Err(err) => Err(SendPacketError::Encode(err)),
            }
        } else {
            Err(SendPacketError::Disconnected)
        }
    }
}

/// Subscribe packet builder
pub struct SubscribeBuilder {
    id: u16,
    shared: Rc<MqttShared>,
    topic_filters: Vec<(ByteString, codec::QoS)>,
}

impl SubscribeBuilder {
    /// Set packet id.
    ///
    /// panics if id is 0
    pub fn packet_id(mut self, id: u16) -> Self {
        if id == 0 {
            panic!("id 0 is not allowed");
        }
        self.id = id;
        self
    }

    /// Add topic filter
    pub fn topic_filter(mut self, filter: ByteString, qos: codec::QoS) -> Self {
        self.topic_filters.push((filter, qos));
        self
    }

    #[allow(clippy::await_holding_refcell_ref)]
    /// Send subscribe packet
    pub async fn send(self) -> Result<Vec<codec::SubscribeReturnCode>, SendPacketError> {
        let shared = self.shared;
        let filters = self.topic_filters;

        if shared.state.is_open() {
            // handle client receive maximum
            if !shared.has_credit() {
                let (tx, rx) = shared.pool.waiters.channel();
                shared.queues.borrow_mut().waiters.push_back(tx);

                if rx.await.is_err() {
                    return Err(SendPacketError::Disconnected);
                }
            }
            let mut queues = shared.queues.borrow_mut();

            // ack channel
            let (tx, rx) = shared.pool.queue.channel();

            // allocate packet id
            let idx = if self.id == 0 { shared.next_id() } else { self.id };
            if queues.inflight.contains_key(&idx) {
                return Err(SendPacketError::PacketIdInUse(idx));
            }
            queues.inflight.insert(idx, (tx, AckType::Subscribe));
            queues.inflight_order.push_back(idx);

            // send subscribe to client
            log::trace!("Sending subscribe packet id: {} filters:{:?}", idx, filters);

            match shared.state.write().encode(
                codec::Packet::Subscribe {
                    packet_id: NonZeroU16::new(idx).unwrap(),
                    topic_filters: filters,
                },
                &shared.codec,
            ) {
                Ok(_) => {
                    // do not borrow cross yield points
                    drop(queues);

                    // wait ack from peer
                    rx.await
                        .map_err(|_| SendPacketError::Disconnected)
                        .map(|pkt| pkt.subscribe())
                }
                Err(err) => Err(SendPacketError::Encode(err)),
            }
        } else {
            Err(SendPacketError::Disconnected)
        }
    }
}

/// Unsubscribe packet builder
pub struct UnsubscribeBuilder {
    id: u16,
    shared: Rc<MqttShared>,
    topic_filters: Vec<ByteString>,
}

impl UnsubscribeBuilder {
    /// Set packet id.
    ///
    /// panics if id is 0
    pub fn packet_id(mut self, id: u16) -> Self {
        if id == 0 {
            panic!("id 0 is not allowed");
        }
        self.id = id;
        self
    }

    /// Add topic filter
    pub fn topic_filter(mut self, filter: ByteString) -> Self {
        self.topic_filters.push(filter);
        self
    }

    #[allow(clippy::await_holding_refcell_ref)]
    /// Send unsubscribe packet
    pub async fn send(self) -> Result<(), SendPacketError> {
        let shared = self.shared;
        let filters = self.topic_filters;

        if shared.state.is_open() {
            // handle client receive maximum
            if !shared.has_credit() {
                let (tx, rx) = shared.pool.waiters.channel();
                shared.queues.borrow_mut().waiters.push_back(tx);

                if rx.await.is_err() {
                    return Err(SendPacketError::Disconnected);
                }
            }
            let mut queues = shared.queues.borrow_mut();

            // ack channel
            let (tx, rx) = shared.pool.queue.channel();

            // allocate packet id
            let idx = if self.id == 0 { shared.next_id() } else { self.id };
            if queues.inflight.contains_key(&idx) {
                return Err(SendPacketError::PacketIdInUse(idx));
            }
            queues.inflight.insert(idx, (tx, AckType::Unsubscribe));
            queues.inflight_order.push_back(idx);

            // send subscribe to client
            log::trace!("Sending unsubscribe packet id: {} filters:{:?}", idx, filters);

            match shared.state.write().encode(
                codec::Packet::Unsubscribe {
                    packet_id: NonZeroU16::new(idx).unwrap(),
                    topic_filters: filters,
                },
                &shared.codec,
            ) {
                Ok(_) => {
                    // do not borrow cross yield points
                    drop(queues);

                    // wait ack from peer
                    rx.await.map_err(|_| SendPacketError::Disconnected).map(|_| ())
                }
                Err(err) => Err(SendPacketError::Encode(err)),
            }
        } else {
            Err(SendPacketError::Disconnected)
        }
    }
}
