#[derive(Clone, PartialEq, prost::Message)]
pub struct Transaction {
    #[prost(message, optional, tag = "1")]
    pub raw_data: Option<Raw>,
    #[prost(bytes = "vec", repeated, tag = "2")]
    pub signature: Vec<Vec<u8>>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Raw {
    #[prost(bytes = "vec", tag = "1")]
    pub ref_block_bytes: Vec<u8>,
    #[prost(bytes = "vec", tag = "4")]
    pub ref_block_hash: Vec<u8>,
    #[prost(int64, tag = "8")]
    pub expiration: i64,
    #[prost(message, repeated, tag = "11")]
    pub contract: Vec<Contract>,
    #[prost(int64, tag = "14")]
    pub timestamp: i64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Contract {
    #[prost(int32, tag = "1")]
    pub kind: i32,
    #[prost(message, optional, tag = "2")]
    pub parameter: Option<Any>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Any {
    #[prost(string, tag = "1")]
    pub type_url: String,
    #[prost(bytes = "vec", tag = "2")]
    pub value: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct TransferContract {
    #[prost(bytes = "vec", tag = "1")]
    pub owner_address: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    pub to_address: Vec<u8>,
    #[prost(int64, tag = "3")]
    pub amount: i64,
}
