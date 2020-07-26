use std::marker::PhantomData;
use std::task::{Context, Poll};

use futures::future::{ok, Ready};
use ntex::service::{Service, ServiceFactory};

use super::control::{ControlPacket, ControlResult};
use super::publish::Publish;
use super::Session;

/// Default publish service
pub struct DefaultPublishService<St, Err> {
    _t: PhantomData<(St, Err)>,
}

impl<St, Err> Default for DefaultPublishService<St, Err> {
    fn default() -> Self {
        Self { _t: PhantomData }
    }
}

impl<St, Err> ServiceFactory for DefaultPublishService<St, Err> {
    type Config = Session<St>;
    type Request = Publish;
    type Response = ();
    type Error = Err;
    type Service = DefaultPublishService<St, Err>;
    type InitError = Err;
    type Future = Ready<Result<Self::Service, Self::InitError>>;

    fn new_service(&self, _: Session<St>) -> Self::Future {
        ok(DefaultPublishService { _t: PhantomData })
    }
}

impl<St, Err> Service for DefaultPublishService<St, Err> {
    type Request = Publish;
    type Response = ();
    type Error = Err;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&self, _: Publish) -> Self::Future {
        log::warn!("Publish service is disabled");
        ok(())
    }
}

/// Default control service
pub struct DefaultControlService<S, E>(PhantomData<(S, E)>);

impl<S, E> Default for DefaultControlService<S, E> {
    fn default() -> Self {
        DefaultControlService(PhantomData)
    }
}

impl<S, E> ServiceFactory for DefaultControlService<S, E> {
    type Config = Session<S>;
    type Request = ControlPacket;
    type Response = ControlResult;
    type Error = E;
    type InitError = E;
    type Service = DefaultControlService<S, E>;
    type Future = Ready<Result<Self::Service, Self::InitError>>;

    fn new_service(&self, _: Session<S>) -> Self::Future {
        ok(DefaultControlService(PhantomData))
    }
}

impl<S, E> Service for DefaultControlService<S, E> {
    type Request = ControlPacket;
    type Response = ControlResult;
    type Error = E;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    #[inline]
    fn poll_ready(&self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    #[inline]
    fn call(&self, subs: ControlPacket) -> Self::Future {
        log::warn!("MQTT Subscribe is not supported");

        ok(match subs {
            ControlPacket::Ping(ping) => ping.ack(),
            ControlPacket::Disconnect(disc) => disc.ack(),
            ControlPacket::Subscribe(subs) => {
                log::warn!("MQTT Subscribe is not supported");
                subs.ack()
            }
            ControlPacket::Unsubscribe(unsubs) => {
                log::warn!("MQTT Unsubscribe is not supported");
                unsubs.ack()
            }
            ControlPacket::Closed(msg) => msg.ack(),
        })
    }
}
