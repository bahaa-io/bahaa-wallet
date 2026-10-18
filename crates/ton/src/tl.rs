const ADNL_QUERY: u32 = 0xb48b_f97a;
const ADNL_ANSWER: u32 = 0x0fac_8416;
const LITE_QUERY: u32 = 0x798c_06df;
const GET_MASTERCHAIN_INFO: u32 = 0x89b5_e62e;
const MASTERCHAIN_INFO: u32 = 0x8583_2881;
const GET_ACCOUNT_STATE: u32 = 0x6b89_0e25;
const ACCOUNT_STATE: u32 = 0x7079_c751;
const RUN_SMC_METHOD: u32 = 0x5cc6_5dd2;
const RUN_METHOD_RESULT: u32 = 0xa39a_616b;
const GET_CONFIG_PARAMS: u32 = 0x2a11_1c19;
const CONFIG_INFO: u32 = 0xae7b_272f;
const SEND_MESSAGE: u32 = 0x690a_d482;
const SEND_MSG_STATUS: u32 = 0x3950_e597;
const GET_TRANSACTIONS: u32 = 0x1c40_e7a1;
const TRANSACTION_LIST: u32 = 0x6f26_c60b;
const ERROR: u32 = 0xbba9_e148;
const RESULT_ONLY: u32 = 4;
const MAX_ERROR: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlockId {
    pub workchain: i32,
    pub shard: i64,
    pub seqno: i32,
    pub root_hash: [u8; 32],
    pub file_hash: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MasterchainInfo {
    pub last: BlockId,
    pub zero_state: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccountState {
    pub block: BlockId,
    pub shard_block: BlockId,
    pub shard_proof: Vec<u8>,
    pub proof: Vec<u8>,
    pub state: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MethodResult {
    pub exit_code: i32,
    pub stack: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigInfo {
    pub block: BlockId,
    pub state_proof: Vec<u8>,
    pub config_proof: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TransactionList {
    pub ids: Vec<BlockId>,
    pub transactions: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Answer<T> {
    Ok(T),
    Refused { code: i32, message: String },
}

#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn u32(&mut self, value: u32) -> &mut Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn i32(&mut self, value: i32) -> &mut Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn i64(&mut self, value: i64) -> &mut Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn int256(&mut self, value: &[u8; 32]) -> &mut Self {
        self.0.extend_from_slice(value);
        self
    }

    fn bytes(&mut self, value: &[u8]) -> &mut Self {
        let start = self.0.len();
        match u8::try_from(value.len()) {
            Ok(short) if short < 254 => self.0.push(short),
            _ => {
                self.0.push(0xfe);
                let length = u32::try_from(value.len()).unwrap_or(u32::MAX).to_le_bytes();
                self.0.extend_from_slice(&length[..3]);
            }
        }
        self.0.extend_from_slice(value);
        while !(self.0.len() - start).is_multiple_of(4) {
            self.0.push(0);
        }
        self
    }

    fn block(&mut self, block: &BlockId) -> &mut Self {
        self.i32(block.workchain)
            .i64(block.shard)
            .i32(block.seqno)
            .int256(&block.root_hash)
            .int256(&block.file_hash)
    }

    fn account(&mut self, workchain: i8, hash: &[u8; 32]) -> &mut Self {
        self.i32(i32::from(workchain)).int256(hash)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(count)?;
        let taken = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(taken)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn int256(&mut self) -> Option<[u8; 32]> {
        self.take(32)?.try_into().ok()
    }

    fn bytes(&mut self) -> Option<&'a [u8]> {
        let start = self.at;
        let first = *self.take(1)?.first()?;
        let length = match first {
            0..=253 => usize::from(first),
            254 => {
                let low = self.take(3)?;
                usize::from(low[0]) | usize::from(low[1]) << 8 | usize::from(low[2]) << 16
            }
            255 => return None,
        };
        let value = self.take(length)?;
        let used = self.at - start;
        self.take((4 - used % 4) % 4)?;
        Some(value)
    }

    fn block(&mut self) -> Option<BlockId> {
        Some(BlockId {
            workchain: self.i32()?,
            shard: self.i64()?,
            seqno: self.i32()?,
            root_hash: self.int256()?,
            file_hash: self.int256()?,
        })
    }

    fn done(&self) -> bool {
        self.at == self.bytes.len()
    }
}

pub(crate) fn query(id: &[u8; 32], function: &[u8]) -> Vec<u8> {
    let mut inner = Writer::default();
    inner.u32(LITE_QUERY).bytes(function);
    let mut outer = Writer::default();
    outer.u32(ADNL_QUERY).int256(id).bytes(&inner.0);
    outer.0
}

pub(crate) fn answer<'a>(id: &[u8; 32], frame: &'a [u8]) -> Option<&'a [u8]> {
    let mut reader = Reader::new(frame);
    if reader.u32()? != ADNL_ANSWER || reader.int256()? != *id {
        return None;
    }
    let answer = reader.bytes()?;
    reader.done().then_some(answer)
}

pub(crate) fn get_masterchain_info() -> Vec<u8> {
    GET_MASTERCHAIN_INFO.to_le_bytes().to_vec()
}

pub(crate) fn get_account_state(block: &BlockId, workchain: i8, hash: &[u8; 32]) -> Vec<u8> {
    let mut writer = Writer::default();
    writer
        .u32(GET_ACCOUNT_STATE)
        .block(block)
        .account(workchain, hash);
    writer.0
}

pub(crate) fn run_smc_method(
    block: &BlockId,
    workchain: i8,
    hash: &[u8; 32],
    method: &str,
    params: &[u8],
) -> Vec<u8> {
    let mut writer = Writer::default();
    writer
        .u32(RUN_SMC_METHOD)
        .u32(RESULT_ONLY)
        .block(block)
        .account(workchain, hash)
        .i64(method_id(method))
        .bytes(params);
    writer.0
}

pub(crate) fn get_config_params(block: &BlockId, params: &[i32]) -> Vec<u8> {
    let mut writer = Writer::default();
    writer
        .u32(GET_CONFIG_PARAMS)
        .u32(0)
        .block(block)
        .u32(u32::try_from(params.len()).unwrap_or(0));
    for param in params {
        writer.i32(*param);
    }
    writer.0
}

pub(crate) fn config_info(answer: &[u8]) -> Option<Answer<ConfigInfo>> {
    read(answer, CONFIG_INFO, |reader| {
        reader.u32()?;
        Some(ConfigInfo {
            block: reader.block()?,
            state_proof: reader.bytes()?.to_vec(),
            config_proof: reader.bytes()?.to_vec(),
        })
    })
}

pub(crate) fn send_message(boc: &[u8]) -> Vec<u8> {
    let mut writer = Writer::default();
    writer.u32(SEND_MESSAGE).bytes(boc);
    writer.0
}

pub(crate) fn get_transactions(
    count: u32,
    workchain: i8,
    hash: &[u8; 32],
    lt: u64,
    last: &[u8; 32],
) -> Vec<u8> {
    let mut writer = Writer::default();
    writer
        .u32(GET_TRANSACTIONS)
        .u32(count)
        .account(workchain, hash)
        .i64(i64::from_le_bytes(lt.to_le_bytes()))
        .int256(last);
    writer.0
}

pub(crate) fn send_status(answer: &[u8]) -> Option<Answer<bool>> {
    read(answer, SEND_MSG_STATUS, |reader| Some(reader.i32()? == 1))
}

pub(crate) fn transaction_list(answer: &[u8]) -> Option<Answer<TransactionList>> {
    read(answer, TRANSACTION_LIST, |reader| {
        let count = reader.u32()?;
        if count > crate::transaction::MAX_LISTED {
            return None;
        }
        let ids = (0..count)
            .map(|_| reader.block())
            .collect::<Option<Vec<_>>>()?;
        Some(TransactionList {
            ids,
            transactions: reader.bytes()?.to_vec(),
        })
    })
}

pub(crate) fn masterchain_info(answer: &[u8]) -> Option<Answer<MasterchainInfo>> {
    read(answer, MASTERCHAIN_INFO, |reader| {
        let last = reader.block()?;
        reader.int256()?;
        let workchain = reader.i32()?;
        let zero_state = reader.int256()?;
        reader.int256()?;
        (workchain == -1).then_some(MasterchainInfo { last, zero_state })
    })
}

pub(crate) fn account_state(answer: &[u8]) -> Option<Answer<AccountState>> {
    read(answer, ACCOUNT_STATE, |reader| {
        Some(AccountState {
            block: reader.block()?,
            shard_block: reader.block()?,
            shard_proof: reader.bytes()?.to_vec(),
            proof: reader.bytes()?.to_vec(),
            state: reader.bytes()?.to_vec(),
        })
    })
}

pub(crate) fn method_result(answer: &[u8]) -> Option<Answer<MethodResult>> {
    read(answer, RUN_METHOD_RESULT, |reader| {
        let mode = reader.u32()?;
        reader.block()?;
        reader.block()?;
        for (bit, fields) in [(1, 2), (2, 1), (8, 1), (16, 1)] {
            if mode & bit != 0 {
                for _ in 0..fields {
                    reader.bytes()?;
                }
            }
        }
        let exit_code = reader.i32()?;
        let stack = if mode & RESULT_ONLY == 0 {
            None
        } else {
            Some(reader.bytes()?.to_vec())
        };
        Some(MethodResult { exit_code, stack })
    })
}

fn read<T>(
    answer: &[u8],
    expected: u32,
    body: impl FnOnce(&mut Reader<'_>) -> Option<T>,
) -> Option<Answer<T>> {
    let mut reader = Reader::new(answer);
    let kind = reader.u32()?;
    let result = if kind == ERROR {
        let code = reader.i32()?;
        let text = String::from_utf8_lossy(reader.bytes()?).into_owned();
        Answer::Refused {
            code,
            message: bahaa_types::to_plain_text(&text, MAX_ERROR),
        }
    } else if kind == expected {
        Answer::Ok(body(&mut reader)?)
    } else {
        return None;
    };
    reader.done().then_some(result)
}

pub(crate) fn method_id(name: &str) -> i64 {
    i64::from(crc16(name.as_bytes())) | 0x1_0000
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for byte in data {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ 0x1021
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crc32(text: &str) -> u32 {
        let mut crc = u32::MAX;
        for byte in text.bytes() {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 == 0 {
                    crc >> 1
                } else {
                    (crc >> 1) ^ 0xedb8_8320
                };
            }
        }
        !crc
    }

    #[test]
    fn constructor_ids_are_the_crc_of_their_schema() {
        let schemas = [
            (
                ADNL_QUERY,
                "adnl.message.query query_id:int256 query:bytes = adnl.Message",
            ),
            (
                ADNL_ANSWER,
                "adnl.message.answer query_id:int256 answer:bytes = adnl.Message",
            ),
            (LITE_QUERY, "liteServer.query data:bytes = Object"),
            (
                GET_MASTERCHAIN_INFO,
                "liteServer.getMasterchainInfo = liteServer.MasterchainInfo",
            ),
            (
                MASTERCHAIN_INFO,
                "liteServer.masterchainInfo last:tonNode.blockIdExt state_root_hash:int256 init:tonNode.zeroStateIdExt = liteServer.MasterchainInfo",
            ),
            (
                GET_ACCOUNT_STATE,
                "liteServer.getAccountState id:tonNode.blockIdExt account:liteServer.accountId = liteServer.AccountState",
            ),
            (
                ACCOUNT_STATE,
                "liteServer.accountState id:tonNode.blockIdExt shardblk:tonNode.blockIdExt shard_proof:bytes proof:bytes state:bytes = liteServer.AccountState",
            ),
            (
                RUN_SMC_METHOD,
                "liteServer.runSmcMethod mode:# id:tonNode.blockIdExt account:liteServer.accountId method_id:long params:bytes = liteServer.RunMethodResult",
            ),
            (
                RUN_METHOD_RESULT,
                "liteServer.runMethodResult mode:# id:tonNode.blockIdExt shardblk:tonNode.blockIdExt shard_proof:mode.0?bytes proof:mode.0?bytes state_proof:mode.1?bytes init_c7:mode.3?bytes lib_extras:mode.4?bytes exit_code:int result:mode.2?bytes = liteServer.RunMethodResult",
            ),
            (
                ERROR,
                "liteServer.error code:int message:string = liteServer.Error",
            ),
            (
                GET_CONFIG_PARAMS,
                "liteServer.getConfigParams mode:# id:tonNode.blockIdExt param_list:(vector int) = liteServer.ConfigInfo",
            ),
            (
                CONFIG_INFO,
                "liteServer.configInfo mode:# id:tonNode.blockIdExt state_proof:bytes config_proof:bytes = liteServer.ConfigInfo",
            ),
            (
                SEND_MESSAGE,
                "liteServer.sendMessage body:bytes = liteServer.SendMsgStatus",
            ),
            (
                SEND_MSG_STATUS,
                "liteServer.sendMsgStatus status:int = liteServer.SendMsgStatus",
            ),
            (
                GET_TRANSACTIONS,
                "liteServer.getTransactions count:# account:liteServer.accountId lt:long hash:int256 = liteServer.TransactionList",
            ),
            (
                TRANSACTION_LIST,
                "liteServer.transactionList ids:(vector tonNode.blockIdExt) transactions:bytes = liteServer.TransactionList",
            ),
        ];
        for (id, schema) in schemas {
            assert_eq!(id, crc32(&schema.replace(['(', ')'], "")), "{schema}");
        }
    }

    #[test]
    fn queries_match_the_adnl_reference_encoding() {
        let id: [u8; 32] = (0..32)
            .map(|at| [0x35, 0x26, 0x3e, 0x6c][at % 4])
            .collect::<Vec<u8>>()
            .try_into()
            .unwrap();
        let encoded = query(&id, &0x16ad_5a34_u32.to_le_bytes());
        assert_eq!(&encoded[..4], [0x7a, 0xf9, 0x8b, 0xb4]);
        assert_eq!(
            &encoded[36..48],
            [
                0x0c, 0xdf, 0x06, 0x8c, 0x79, 0x04, 0x34, 0x5a, 0xad, 0x16, 0, 0
            ]
        );
        assert_eq!(&encoded[48..], [0, 0, 0, 0]);
    }

    #[test]
    fn bytes_round_trip_with_short_and_long_lengths() {
        for length in [0, 1, 3, 4, 253, 254, 300, 70_000] {
            let value: Vec<u8> = (0..length)
                .map(|at| u8::try_from(at % 251).unwrap())
                .collect();
            let mut writer = Writer::default();
            writer.bytes(&value).u32(7);
            assert!(writer.0.len().is_multiple_of(4));
            let mut reader = Reader::new(&writer.0);
            assert_eq!(reader.bytes().unwrap(), value.as_slice(), "{length}");
            assert_eq!(reader.u32(), Some(7));
            assert!(reader.done());
        }
        assert_eq!(Reader::new(&[0xff, 0, 0, 0]).bytes(), None);
        assert_eq!(Reader::new(&[0x09, 1, 2, 3]).bytes(), None);
    }

    fn block() -> BlockId {
        BlockId {
            workchain: -1,
            shard: i64::MIN,
            seqno: 99_766_277,
            root_hash: [1; 32],
            file_hash: [2; 32],
        }
    }

    #[test]
    fn answers_are_read_strictly() {
        let mut info = Writer::default();
        info.u32(MASTERCHAIN_INFO)
            .block(&block())
            .int256(&[3; 32])
            .i32(-1)
            .int256(&[4; 32])
            .int256(&[5; 32]);
        assert_eq!(
            masterchain_info(&info.0),
            Some(Answer::Ok(MasterchainInfo {
                last: block(),
                zero_state: [4; 32]
            }))
        );
        let mut longer = info.0.clone();
        longer.push(0);
        assert_eq!(masterchain_info(&longer), None);
        assert_eq!(masterchain_info(&info.0[..40]), None);
        let mut error = Writer::default();
        error
            .u32(ERROR)
            .i32(651)
            .bytes(b"state not ready \xff\xfe\x00 for \xe2\x80\xae block");
        assert_eq!(
            masterchain_info(&error.0),
            Some(Answer::Refused {
                code: 651,
                message: "state not ready \u{fffd}\u{fffd} for block".to_owned()
            })
        );
        let mut state = Writer::default();
        state
            .u32(ACCOUNT_STATE)
            .block(&block())
            .block(&block())
            .bytes(b"p1")
            .bytes(b"p2")
            .bytes(b"boc");
        assert_eq!(
            account_state(&state.0),
            Some(Answer::Ok(AccountState {
                block: block(),
                shard_block: block(),
                shard_proof: b"p1".to_vec(),
                proof: b"p2".to_vec(),
                state: b"boc".to_vec()
            }))
        );
        assert_eq!(account_state(&info.0), None);
    }

    #[test]
    fn method_results_skip_only_the_fields_their_mode_has() {
        let mut result = Writer::default();
        result
            .u32(RUN_METHOD_RESULT)
            .u32(4 | 1)
            .block(&block())
            .block(&block())
            .bytes(b"shard")
            .bytes(b"proof")
            .i32(0)
            .bytes(b"stack");
        assert_eq!(
            method_result(&result.0),
            Some(Answer::Ok(MethodResult {
                exit_code: 0,
                stack: Some(b"stack".to_vec())
            }))
        );
        let mut bare = Writer::default();
        bare.u32(RUN_METHOD_RESULT)
            .u32(0)
            .block(&block())
            .block(&block())
            .i32(-13);
        assert_eq!(
            method_result(&bare.0),
            Some(Answer::Ok(MethodResult {
                exit_code: -13,
                stack: None
            }))
        );
    }

    #[test]
    fn method_ids_are_the_standard_ones() {
        assert_eq!(method_id("seqno"), 85_143);
        assert_eq!(method_id("get_wallet_address"), 103_289);
        assert_eq!(method_id("get_wallet_data"), 97_026);
    }

    #[test]
    fn answers_belong_to_their_query() {
        let mut frame = Writer::default();
        frame.u32(ADNL_ANSWER).int256(&[9; 32]).bytes(b"hello");
        assert_eq!(answer(&[9; 32], &frame.0), Some(b"hello".as_slice()));
        assert_eq!(answer(&[8; 32], &frame.0), None);
    }

    #[test]
    fn history_and_sending_have_their_reference_layout() {
        let asked = get_transactions(16, 0, &[0x13; 32], 109_578_663_000_001, &[0x5b; 32]);
        assert_eq!(asked[..4], GET_TRANSACTIONS.to_le_bytes());
        assert_eq!(asked[4..8], 16u32.to_le_bytes());
        assert_eq!(asked[8..12], 0i32.to_le_bytes());
        assert_eq!(asked[12..44], [0x13; 32]);
        assert_eq!(asked[44..52], 109_578_663_000_001u64.to_le_bytes());
        assert_eq!(asked[52..], [0x5b; 32]);
        let sent = send_message(b"boc");
        assert_eq!(sent[..4], SEND_MESSAGE.to_le_bytes());
        assert_eq!(sent[4..], [3, b'b', b'o', b'c']);
        for (status, taken) in [(1, true), (0, false)] {
            let mut answer = Writer::default();
            answer.u32(SEND_MSG_STATUS).i32(status);
            assert_eq!(send_status(&answer.0), Some(Answer::Ok(taken)));
        }
        let listed = |count: u32| {
            let mut answer = Writer::default();
            answer.u32(TRANSACTION_LIST).u32(count);
            for _ in 0..count {
                answer.block(&block());
            }
            answer.bytes(b"txs");
            answer.0
        };
        assert_eq!(
            transaction_list(&listed(16)),
            Some(Answer::Ok(TransactionList {
                ids: vec![block(); 16],
                transactions: b"txs".to_vec()
            }))
        );
        assert_eq!(transaction_list(&listed(17)), None);
        let mut huge = Writer::default();
        huge.u32(TRANSACTION_LIST).u32(u32::MAX);
        assert_eq!(transaction_list(&huge.0), None);
    }
}
