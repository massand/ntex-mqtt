#![allow(clippy::type_complexity, clippy::new_ret_no_self)]
//! MQTT Client/Server framework

#[macro_use]
mod topic;
#[macro_use]
mod utils;

pub mod error;
pub mod v3;
pub mod v5;

mod handshake;
mod router;
mod server;
mod service;
mod session;
pub mod types;
mod version;

pub use self::error::MqttError;
pub use self::router::Router;
pub use self::server::MqttServer;
pub use self::session::Session;
pub use self::topic::Topic;

// http://www.iana.org/assignments/service-names-port-numbers/service-names-port-numbers.xhtml
pub const TCP_PORT: u16 = 1883;
pub const SSL_PORT: u16 = 8883;
