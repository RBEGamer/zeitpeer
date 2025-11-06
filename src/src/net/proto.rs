use serde::{Deserialize, Serialize};

pub const MSG_REQ: u8 = 1;
pub const MSG_RESP: u8 = 2;
pub const WIRE_VERSION: u8 = 1;
pub const SIGNATURE_BYTES: usize = 32;
pub const RESERVED_BYTES: usize = 16;

pub type Signature = [u8; SIGNATURE_BYTES];
pub type Reserved = [u8; RESERVED_BYTES];

/// Packet header shared between request/response.
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct Header {
    pub ver: u8,
    pub kind: u8,
    pub seq: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct TimeRequest {
    pub header: Header,
    pub sender_id: u32,
    pub t1_send_ns: u64,
    pub reserved: Reserved,
    pub signature: Signature,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct TimeResponse {
    pub header: Header,
    pub server_id: u32,
    pub t1_echo_ns: u64, // from request
    pub t2_recv_ns: u64, // server rx
    pub t3_send_ns: u64, // server tx
    pub server_quality: u32,
    pub reserved: Reserved,
    pub signature: Signature,
}

impl Header {
    pub fn new(kind: u8, seq: u32) -> Self {
        Header {
            ver: WIRE_VERSION,
            kind,
            seq,
        }
    }
}
