const ADNL_QUERY: u32 = 0xb48b_f97a;
const ADNL_ANSWER: u32 = 0x0fac_8416;
const LITE_QUERY: u32 = 0x798c_06df;
const GET_MASTERCHAIN_INFO: u32 = 0x89b5_e62e;
const MASTERCHAIN_INFO: u32 = 0x8583_2881;
const GET_ACCOUNT_STATE: u32 = 0x6b89_0e25;
const ACCOUNT_STATE: u32 = 0x7079_c751;
const GET_CONFIG_PARAMS: u32 = 0x2a11_1c19;
const CONFIG_INFO: u32 = 0xae7b_272f;
const SEND_MESSAGE: u32 = 0x690a_d482;
const SEND_MSG_STATUS: u32 = 0x3950_e597;
const GET_TRANSACTIONS: u32 = 0x1c40_e7a1;
const TRANSACTION_LIST: u32 = 0x6f26_c60b;
const GET_LIBRARIES: u32 = 0xd122_b662;
const LIBRARY_RESULT: u32 = 0x117a_b96b;
const MAX_LIBRARY_ENTRIES: u32 = 16;
const GET_BLOCK_PROOF: u32 = 0x8aea_9c44;
const LOOKUP_BLOCK: u32 = 0xfac8_f71e;
const BLOCK_HEADER: u32 = 0x752d_8219;
const BY_SEQNO: u32 = 1;
const TO_TARGET: u32 = 0x1001;
const PARTIAL_BLOCK_PROOF: u32 = 0x8ed0_d2c1;
const BLOCK_LINK_FORWARD: u32 = 0x520f_ce1c;
const ORDINARY_SIGNATURES: u32 = 0xf644_a6e6;
const SIMPLEX_SIGNATURES: u32 = 0xac24_9800;
const BLOCK_SIGNED: u32 = 0xc50b_6e70;
const DATA_TO_SIGN: u32 = 0xa8e3_3df8;
const FINALIZE_VOTE: u32 = 0x40a7_e105;
const CANDIDATE_ID: u32 = 0xb691_cd3f;
const CANDIDATE_ORDINARY: u32 = 0xe8f9_bcdc;
const CANDIDATE_EMPTY: u32 = 0x72b4_d933;
const CANDIDATE_PARENT: u32 = 0x1a4b_9af1;
const WITHOUT_PARENTS: u32 = 0x22cb_cca9;
const BOOL_TRUE: u32 = 0x9972_75b5;
const BOOL_FALSE: u32 = 0xbc79_9737;
const MAX_STEPS: u32 = 16;
const MAX_SIGNATURES: u32 = 1024;
const ERROR: u32 = 0xbba9_e148;
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
pub(crate) struct ConfigInfo {
    pub block: BlockId,
    pub state_proof: Vec<u8>,
    pub config_proof: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TransactionList {
    pub transactions: Vec<u8>,
}

pub(crate) type Libraries = Vec<([u8; 32], Vec<u8>)>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Simplex {
    pub session_id: [u8; 32],
    pub slot: u32,
    pub candidate: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Signed {
    pub validator_set_hash: u32,
    pub catchain_seqno: u32,
    pub signatures: Vec<([u8; 32], Vec<u8>)>,
    pub simplex: Option<Simplex>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Link {
    pub to_key_block: bool,
    pub from: BlockId,
    pub to: BlockId,
    pub dest_proof: Vec<u8>,
    pub config_proof: Vec<u8>,
    pub signed: Signed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlockProof {
    pub complete: bool,
    pub from: BlockId,
    pub to: BlockId,
    pub steps: Vec<Link>,
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

    fn bool(&mut self) -> Option<bool> {
        match self.u32()? {
            BOOL_TRUE => Some(true),
            BOOL_FALSE => Some(false),
            _ => None,
        }
    }

    fn link(&mut self) -> Option<Link> {
        if self.u32()? != BLOCK_LINK_FORWARD {
            return None;
        }
        Some(Link {
            to_key_block: self.bool()?,
            from: self.block()?,
            to: self.block()?,
            dest_proof: self.bytes()?.to_vec(),
            config_proof: self.bytes()?.to_vec(),
            signed: self.signed()?,
        })
    }

    fn signed(&mut self) -> Option<Signed> {
        let kind = self.u32()?;
        let (validator_set_hash, catchain_seqno) = match kind {
            ORDINARY_SIGNATURES => (self.u32()?, self.u32()?),
            SIMPLEX_SIGNATURES => {
                let catchain_seqno = self.u32()?;
                (self.u32()?, catchain_seqno)
            }
            _ => return None,
        };
        let count = self.u32()?;
        if count > MAX_SIGNATURES {
            return None;
        }
        let signatures = (0..count)
            .map(|_| Some((self.int256()?, self.bytes()?.to_vec())))
            .collect::<Option<Vec<_>>>()?;
        let simplex = if kind == SIMPLEX_SIGNATURES {
            Some(Simplex {
                session_id: self.int256()?,
                slot: self.u32()?,
                candidate: self.bytes()?.to_vec(),
            })
        } else {
            None
        };
        Some(Signed {
            validator_set_hash,
            catchain_seqno,
            signatures,
            simplex,
        })
    }

    fn candidate(&mut self) -> Option<BlockId> {
        let kind = self.u32()?;
        let block = self.block()?;
        match kind {
            CANDIDATE_ORDINARY => {
                self.int256()?;
                match self.u32()? {
                    CANDIDATE_PARENT => self.candidate_id()?,
                    WITHOUT_PARENTS => {}
                    _ => return None,
                }
            }
            CANDIDATE_EMPTY => {
                self.u32()?;
                self.int256()?;
            }
            _ => return None,
        }
        Some(block)
    }

    fn candidate_id(&mut self) -> Option<()> {
        (self.u32()? == CANDIDATE_ID).then_some(())?;
        self.u32()?;
        self.int256()?;
        Some(())
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

pub(crate) fn get_block_proof(known: &BlockId, target: &BlockId) -> Vec<u8> {
    let mut writer = Writer::default();
    writer
        .u32(GET_BLOCK_PROOF)
        .u32(TO_TARGET)
        .block(known)
        .block(target);
    writer.0
}

pub(crate) fn lookup_block(workchain: i32, shard: i64, seqno: i32) -> Vec<u8> {
    let mut writer = Writer::default();
    writer
        .u32(LOOKUP_BLOCK)
        .u32(BY_SEQNO)
        .i32(workchain)
        .i64(shard)
        .i32(seqno);
    writer.0
}

pub(crate) fn block_signed(block: &BlockId) -> Vec<u8> {
    let mut writer = Writer::default();
    writer
        .u32(BLOCK_SIGNED)
        .int256(&block.root_hash)
        .int256(&block.file_hash);
    writer.0
}

pub(crate) fn finalized(session_id: &[u8; 32], slot: u32, candidate: &[u8; 32]) -> Vec<u8> {
    let mut vote = Writer::default();
    vote.u32(FINALIZE_VOTE)
        .u32(CANDIDATE_ID)
        .u32(slot)
        .int256(candidate);
    let mut writer = Writer::default();
    writer.u32(DATA_TO_SIGN).int256(session_id).bytes(&vote.0);
    writer.0
}

pub(crate) fn candidate_block(candidate: &[u8]) -> Option<BlockId> {
    let mut reader = Reader::new(candidate);
    let block = reader.candidate()?;
    reader.done().then_some(block)
}

pub(crate) fn block_proof(answer: &[u8]) -> Option<Answer<BlockProof>> {
    read(answer, PARTIAL_BLOCK_PROOF, |reader| {
        let complete = reader.bool()?;
        let from = reader.block()?;
        let to = reader.block()?;
        let count = reader.u32()?;
        if count > MAX_STEPS {
            return None;
        }
        let steps = (0..count)
            .map(|_| reader.link())
            .collect::<Option<Vec<_>>>()?;
        Some(BlockProof {
            complete,
            from,
            to,
            steps,
        })
    })
}

pub(crate) fn get_libraries(hashes: &[[u8; 32]]) -> Vec<u8> {
    let mut writer = Writer::default();
    writer
        .u32(GET_LIBRARIES)
        .u32(u32::try_from(hashes.len()).unwrap_or(u32::MAX));
    for hash in hashes {
        writer.int256(hash);
    }
    writer.0
}

pub(crate) fn library_result(answer: &[u8]) -> Option<Answer<Libraries>> {
    read(answer, LIBRARY_RESULT, |reader| {
        let count = reader.u32()?;
        if count > MAX_LIBRARY_ENTRIES {
            return None;
        }
        (0..count)
            .map(|_| Some((reader.int256()?, reader.bytes()?.to_vec())))
            .collect()
    })
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
        for _ in 0..count {
            reader.block()?;
        }
        Some(TransactionList {
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

pub(crate) fn block_header(answer: &[u8]) -> Option<Answer<BlockId>> {
    read(answer, BLOCK_HEADER, |reader| {
        let id = reader.block()?;
        reader.u32()?;
        reader.bytes()?;
        Some(id)
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
            (
                GET_LIBRARIES,
                "liteServer.getLibraries library_list:(vector int256) = liteServer.LibraryResult",
            ),
        ];
        named_by_their_schema(&schemas);
    }

    #[test]
    fn block_proof_ids_are_the_crc_of_their_schema() {
        let schemas = [
            (
                GET_BLOCK_PROOF,
                "liteServer.getBlockProof mode:# known_block:tonNode.blockIdExt target_block:mode.0?tonNode.blockIdExt = liteServer.PartialBlockProof",
            ),
            (
                PARTIAL_BLOCK_PROOF,
                "liteServer.partialBlockProof complete:Bool from:tonNode.blockIdExt to:tonNode.blockIdExt steps:(vector liteServer.BlockLink) = liteServer.PartialBlockProof",
            ),
            (
                BLOCK_LINK_FORWARD,
                "liteServer.blockLinkForward to_key_block:Bool from:tonNode.blockIdExt to:tonNode.blockIdExt dest_proof:bytes config_proof:bytes signatures:liteServer.SignatureSet = liteServer.BlockLink",
            ),
            (
                ORDINARY_SIGNATURES,
                "liteServer.signatureSet validator_set_hash:int catchain_seqno:int signatures:(vector liteServer.signature) = liteServer.SignatureSet",
            ),
            (
                SIMPLEX_SIGNATURES,
                "liteServer.signatureSet.simplex cc_seqno:int validator_set_hash:int signatures:(vector liteServer.signature) session_id:int256 slot:int candidate:bytes = liteServer.SignatureSet",
            ),
            (
                BLOCK_SIGNED,
                "ton.blockId root_cell_hash:int256 file_hash:int256 = ton.BlockId",
            ),
            (
                DATA_TO_SIGN,
                "consensus.dataToSign session_id:int256 data:bytes = consensus.DataToSign",
            ),
            (
                FINALIZE_VOTE,
                "consensus.simplex.finalizeVote id:consensus.CandidateId = consensus.simplex.UnsignedVote",
            ),
            (
                CANDIDATE_ID,
                "consensus.candidateId slot:int hash:int256 = consensus.CandidateId",
            ),
            (
                CANDIDATE_ORDINARY,
                "consensus.candidateHashDataOrdinary block:tonNode.blockIdExt collated_file_hash:int256 parent:consensus.CandidateParent = consensus.CandidateHashData",
            ),
            (
                CANDIDATE_EMPTY,
                "consensus.candidateHashDataEmpty block:tonNode.blockIdExt parent:consensus.candidateId = consensus.CandidateHashData",
            ),
            (
                CANDIDATE_PARENT,
                "consensus.candidateParent id:consensus.CandidateId = consensus.CandidateParent",
            ),
            (
                WITHOUT_PARENTS,
                "consensus.candidateWithoutParents = consensus.CandidateParent",
            ),
            (BOOL_TRUE, "boolTrue = Bool"),
            (BOOL_FALSE, "boolFalse = Bool"),
            (
                LIBRARY_RESULT,
                "liteServer.libraryResult result:(vector liteServer.libraryEntry) = liteServer.LibraryResult",
            ),
        ];
        named_by_their_schema(&schemas);
    }

    fn named_by_their_schema(schemas: &[(u32, &str)]) {
        for (id, schema) in schemas {
            assert_eq!(*id, crc32(&schema.replace(['(', ')'], "")), "{schema}");
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
    fn an_older_block_is_looked_up_by_its_seqno() {
        let asked = lookup_block(-1, i64::MIN, 99_971_206);
        assert_eq!(asked[..4], LOOKUP_BLOCK.to_le_bytes());
        assert_eq!(asked[4..8], 1u32.to_le_bytes());
        assert_eq!(asked[8..12], (-1i32).to_le_bytes());
        assert_eq!(asked[12..20], i64::MIN.to_le_bytes());
        assert_eq!(asked[20..], 99_971_206i32.to_le_bytes());
        let mut header = Writer::default();
        header
            .u32(BLOCK_HEADER)
            .block(&block())
            .u32(1)
            .bytes(b"proof");
        assert_eq!(block_header(&header.0), Some(Answer::Ok(block())));
        let mut longer = header.0.clone();
        longer.push(0);
        assert_eq!(block_header(&longer), None);
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
                transactions: b"txs".to_vec()
            }))
        );
        assert_eq!(transaction_list(&listed(17)), None);
        let mut huge = Writer::default();
        huge.u32(TRANSACTION_LIST).u32(u32::MAX);
        assert_eq!(transaction_list(&huge.0), None);
    }

    #[test]
    fn libraries_are_asked_by_hash_and_answered_with_their_cells() {
        let asked = get_libraries(&[[0x8f; 32], [0x45; 32]]);
        assert_eq!(asked[..4], GET_LIBRARIES.to_le_bytes());
        assert_eq!(asked[4..8], 2u32.to_le_bytes());
        assert_eq!(asked[8..40], [0x8f; 32]);
        assert_eq!(asked[40..], [0x45; 32]);
        let mut answer = Writer::default();
        answer
            .u32(LIBRARY_RESULT)
            .u32(1)
            .int256(&[0x8f; 32])
            .bytes(b"boc");
        assert_eq!(
            library_result(&answer.0),
            Some(Answer::Ok(vec![([0x8f; 32], b"boc".to_vec())]))
        );
        let mut huge = Writer::default();
        huge.u32(LIBRARY_RESULT).u32(17);
        assert_eq!(library_result(&huge.0), None);
    }

    #[test]
    fn block_proofs_are_asked_to_the_target_and_read_strictly() {
        let known = block();
        let mut target = block();
        target.seqno += 5;
        let asked = get_block_proof(&known, &target);
        assert_eq!(asked[..4], GET_BLOCK_PROOF.to_le_bytes());
        assert_eq!(asked[4..8], TO_TARGET.to_le_bytes());
        assert_eq!(asked.len(), 8 + 80 + 80);
        assert_eq!(asked[8 + 12..8 + 16], known.seqno.to_le_bytes());
        assert_eq!(asked[88 + 12..88 + 16], target.seqno.to_le_bytes());
        let answer = |link: u32, signatures: u32, steps: u32| {
            let mut writer = Writer::default();
            writer
                .u32(PARTIAL_BLOCK_PROOF)
                .u32(BOOL_TRUE)
                .block(&known)
                .block(&target)
                .u32(steps);
            for _ in 0..steps.min(1) {
                writer
                    .u32(link)
                    .u32(BOOL_FALSE)
                    .block(&known)
                    .block(&target)
                    .bytes(b"dest")
                    .bytes(b"config")
                    .u32(signatures)
                    .u32(7)
                    .u32(9)
                    .u32(1)
                    .int256(&[3; 32])
                    .bytes(&[4; 64]);
            }
            writer.0
        };
        let Some(Answer::Ok(read)) =
            block_proof(&answer(BLOCK_LINK_FORWARD, ORDINARY_SIGNATURES, 1))
        else {
            panic!("an ordinary link should read");
        };
        assert_eq!(read.steps[0].signed.validator_set_hash, 7);
        assert_eq!(read.steps[0].signed.catchain_seqno, 9);
        assert_eq!(read.steps[0].signed.simplex, None);
        assert_eq!(
            block_proof(&answer(0xef7e_1bef, ORDINARY_SIGNATURES, 1)),
            None
        );
        assert_eq!(
            block_proof(&answer(BLOCK_LINK_FORWARD, 0x1234_5678, 1)),
            None
        );
        assert_eq!(
            block_proof(&answer(BLOCK_LINK_FORWARD, SIMPLEX_SIGNATURES, 1)),
            None
        );
        assert_eq!(
            block_proof(&answer(
                BLOCK_LINK_FORWARD,
                ORDINARY_SIGNATURES,
                MAX_STEPS + 1
            )),
            None
        );
    }

    #[test]
    fn every_candidate_form_names_its_block() {
        let target = block();
        let parent = |writer: &mut Writer| {
            writer.u32(CANDIDATE_ID).u32(41).int256(&[6; 32]);
        };
        let mut with_parent = Writer::default();
        with_parent
            .u32(CANDIDATE_ORDINARY)
            .block(&target)
            .int256(&[5; 32])
            .u32(CANDIDATE_PARENT);
        parent(&mut with_parent);
        let mut first = Writer::default();
        first
            .u32(CANDIDATE_ORDINARY)
            .block(&target)
            .int256(&[5; 32])
            .u32(WITHOUT_PARENTS);
        let mut empty = Writer::default();
        empty
            .u32(CANDIDATE_EMPTY)
            .block(&target)
            .u32(41)
            .int256(&[6; 32]);
        for candidate in [&with_parent.0, &first.0, &empty.0] {
            assert_eq!(candidate_block(candidate), Some(target));
            let mut longer = candidate.clone();
            longer.extend_from_slice(&[0; 4]);
            assert_eq!(candidate_block(&longer), None);
            assert_eq!(candidate_block(&candidate[..candidate.len() - 4]), None);
        }
        let mut foreign = with_parent.0.clone();
        foreign[..4].copy_from_slice(&0x1111_1111_u32.to_le_bytes());
        assert_eq!(candidate_block(&foreign), None);
        let mut unparented = with_parent.0.clone();
        let at = 4 + 80 + 32;
        unparented[at..at + 4].copy_from_slice(&0x2222_2222_u32.to_le_bytes());
        assert_eq!(candidate_block(&unparented), None);
        let finalized = finalized(&[8; 32], 41, &[9; 32]);
        assert_eq!(finalized.len(), 4 + 32 + 48);
        assert_eq!(finalized[36], 44);
        assert_eq!(finalized[37..41], FINALIZE_VOTE.to_le_bytes());
        assert_eq!(finalized[41..45], CANDIDATE_ID.to_le_bytes());
        assert_eq!(block_signed(&target)[..4], BLOCK_SIGNED.to_le_bytes());
    }
}
