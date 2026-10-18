#[derive(Clone, PartialEq, Eq, prost::Message)]
pub(crate) struct Transaction {
    #[prost(message, optional, tag = "1")]
    pub raw_data: Option<Raw>,
    #[prost(bytes = "vec", repeated, tag = "2")]
    pub signature: Vec<Vec<u8>>,
}

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub(crate) struct Raw {
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
    #[prost(int64, tag = "18")]
    pub fee_limit: i64,
}

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub(crate) struct Contract {
    #[prost(int32, tag = "1")]
    pub kind: i32,
    #[prost(message, optional, tag = "2")]
    pub parameter: Option<Any>,
}

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub(crate) struct Any {
    #[prost(string, tag = "1")]
    pub type_url: String,
    #[prost(bytes = "vec", tag = "2")]
    pub value: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub(crate) struct TransferContract {
    #[prost(bytes = "vec", tag = "1")]
    pub owner_address: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    pub to_address: Vec<u8>,
    #[prost(int64, tag = "3")]
    pub amount: i64,
}

#[derive(Clone, PartialEq, Eq, prost::Message)]
pub(crate) struct TriggerSmartContract {
    #[prost(bytes = "vec", tag = "1")]
    pub owner_address: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    pub contract_address: Vec<u8>,
    #[prost(int64, tag = "3")]
    pub call_value: i64,
    #[prost(bytes = "vec", tag = "4")]
    pub data: Vec<u8>,
    #[prost(int64, tag = "5")]
    pub call_token_value: i64,
    #[prost(int64, tag = "6")]
    pub token_id: i64,
}
