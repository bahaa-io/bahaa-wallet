use bahaa_chain::{ChainError, Failure, Pool, http};
use bahaa_config::Endpoint;
use bahaa_types::{Address, ChainFamily};
use dnssec_prover::query::{ProofBuilder, ProofBuildingError, QueryBuf};
use dnssec_prover::rr::{Name, RR, TXT_TYPE};
use dnssec_prover::ser::parse_rr_stream;
use dnssec_prover::validation::{ValidationError, verify_rr_stream};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::{Client, StatusCode};

const SCHEME: &str = "bitcoin:";
const DNS_MESSAGE: &str = "application/dns-message";
const SIGN: char = '\u{20bf}';
const RSA: [u8; 2] = [8, 10];
const MIN_RSA_BITS: usize = 1024;
const MAX_QUERIES: usize = 24;
const MAX_LABEL: usize = 63;
const MAX_NAME: usize = 253;
const INVALID: ChainError = ChainError::Invalid("this is not a valid Bitcoin name");
const UNPROVEN: ChainError = ChainError::Invalid("this name's DNSSEC proof doesn't check out");
const UNPAYABLE: ChainError =
    ChainError::Invalid("this name gives no Bitcoin address this wallet can pay");

#[derive(Debug)]
pub(crate) struct Resolver {
    pool: Pool,
    http: Client,
}

impl Resolver {
    pub(crate) fn new(endpoints: Vec<Endpoint>) -> Result<Self, ChainError> {
        Ok(Self {
            pool: Pool::new(endpoints),
            http: http::client()?,
        })
    }

    pub(crate) async fn address(
        &self,
        name: &str,
        now: u64,
    ) -> Result<Option<Address>, ChainError> {
        let asked = record_name(name)?;
        let (mut builder, first) = ProofBuilder::new(&asked, TXT_TYPE);
        let mut pending = vec![first];
        let mut queries = 0;
        while builder.awaiting_responses() {
            let query = pending.pop().ok_or_else(ChainError::malformed)?;
            queries += 1;
            if queries > MAX_QUERIES {
                return Err(ChainError::Network(
                    "the name needed too many lookups".to_owned(),
                ));
            }
            let reply = self.ask(&query.into_vec()).await?;
            let mut answer = QueryBuf::new_zeroed(0);
            answer.extend_from_slice(&reply);
            match builder.process_response(&answer) {
                Ok(more) => pending.extend(more),
                Err(ProofBuildingError::NoSuchName) => return Ok(None),
                Err(ProofBuildingError::Unauthenticated) => {
                    return Err(ChainError::Invalid(
                        "this name's domain isn't signed with DNSSEC, so it can't be checked",
                    ));
                }
                Err(_) => {
                    return Err(ChainError::Network(
                        "the name server sent an answer that can't be read".to_owned(),
                    ));
                }
            }
        }
        let (proof, _) = builder
            .finish_proof()
            .map_err(|()| ChainError::malformed())?;
        read_proof(&proof, &asked, now)
    }

    async fn ask(&self, query: &[u8]) -> Result<Vec<u8>, ChainError> {
        Ok(self
            .pool
            .call(None, |endpoint| async move {
                let request = self
                    .http
                    .post(endpoint.url)
                    .header(CONTENT_TYPE, DNS_MESSAGE)
                    .header(ACCEPT, DNS_MESSAGE)
                    .body(query.to_vec());
                let (status, body) = http::exchange(request).await?;
                if status != StatusCode::OK || body.len() > usize::from(u16::MAX) {
                    return Err(Failure::Retry(ChainError::Network(format!(
                        "the name server answered HTTP {status}"
                    ))));
                }
                Ok(body)
            })
            .await?)
    }
}

pub fn normalize(name: &str) -> Result<String, ChainError> {
    let lower = name.trim().trim_start_matches(SIGN).to_ascii_lowercase();
    let (user, domain) = lower.split_once('@').ok_or(INVALID)?;
    let label = |label: &str, extra: &[u8]| {
        (1..=MAX_LABEL).contains(&label.len())
            && label.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || extra.contains(&byte)
            })
    };
    let fine = label(user, b"-_")
        && domain.contains('.')
        && domain.len() <= MAX_NAME
        && domain.split('.').all(|part| label(part, b"-_"));
    if fine {
        Ok(format!("{SIGN}{lower}"))
    } else {
        Err(INVALID)
    }
}

fn record_name(name: &str) -> Result<Name, ChainError> {
    let name = normalize(name)?;
    let (user, domain) = name
        .trim_start_matches(SIGN)
        .split_once('@')
        .ok_or(INVALID)?;
    Name::try_from(format!("{user}.user._bitcoin-payment.{domain}.")).map_err(|()| INVALID)
}

fn read_proof(proof: &[u8], asked: &Name, now: u64) -> Result<Option<Address>, ChainError> {
    let records = parse_rr_stream(proof).map_err(|()| UNPROVEN)?;
    if records.iter().any(weak) {
        return Err(ChainError::Invalid(
            "this name's domain is signed with a key too weak to trust",
        ));
    }
    let verified = verify_rr_stream(&records).map_err(|err| match err {
        ValidationError::UnsupportedAlgorithm => {
            ChainError::Invalid("this name's domain is signed in a way this wallet can't check")
        }
        ValidationError::Invalid | ValidationError::ValidationCountLimited => UNPROVEN,
    })?;
    if now < verified.valid_from || now > verified.expires {
        return Err(UNPROVEN);
    }
    let mut offers = verified
        .resolve_name(asked)
        .into_iter()
        .filter_map(|record| match record {
            RR::Txt(txt) => Some(txt.data.as_vec()),
            _ => None,
        })
        .filter(|text| {
            text.get(..SCHEME.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(SCHEME.as_bytes()))
        });
    match (offers.next(), offers.next()) {
        (None, _) => Ok(None),
        (Some(offer), None) => onchain(&String::from_utf8(offer).map_err(|_| UNPAYABLE)?).map(Some),
        (Some(_), Some(_)) => Err(ChainError::Invalid(
            "this name gives more than one Bitcoin payment instruction",
        )),
    }
}

fn weak(record: &RR) -> bool {
    let RR::DnsKey(key) = record else {
        return false;
    };
    if !RSA.contains(&key.alg) {
        return false;
    }
    let modulus = match key.pubkey.split_first() {
        Some((0, rest)) => rest.get(2..).and_then(|after| {
            let length = usize::from(u16::from_be_bytes([rest[0], rest[1]]));
            after.get(length..)
        }),
        Some((length, rest)) => rest.get(usize::from(*length)..),
        None => None,
    };
    let Some(modulus) = modulus else {
        return true;
    };
    let significant: Vec<u8> = modulus
        .iter()
        .copied()
        .skip_while(|byte| *byte == 0)
        .collect();
    let bits = significant.first().map_or(0, |top| {
        (significant.len() - 1) * 8 + (8 - top.leading_zeros() as usize)
    });
    bits < MIN_RSA_BITS
}

fn onchain(uri: &str) -> Result<Address, ChainError> {
    let rest = uri
        .get(..SCHEME.len())
        .filter(|start| start.eq_ignore_ascii_case(SCHEME))
        .and_then(|_| uri.get(SCHEME.len()..))
        .ok_or(UNPAYABLE)?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut found = Vec::new();
    if !path.is_empty() {
        found.push(mainnet(path)?);
    }
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = key.to_ascii_lowercase();
        if key.starts_with("req-") {
            return Err(ChainError::Invalid(
                "this name asks for a payment feature this wallet doesn't have",
            ));
        }
        if key == "bc" {
            let address = mainnet(value)?;
            if !matches!(address, Address::Bitcoin { script, .. } if script.is_witness()) {
                return Err(UNPAYABLE);
            }
            found.push(address);
        }
    }
    found.into_iter().next().ok_or(UNPAYABLE)
}

fn mainnet(text: &str) -> Result<Address, ChainError> {
    match Address::parse(ChainFamily::Bitcoin, text) {
        Ok(address @ Address::Bitcoin { testnet: false, .. }) => Ok(address),
        _ => Err(UNPAYABLE),
    }
}

#[cfg(fuzzing)]
pub(crate) fn fuzz(bytes: &[u8]) {
    if let Ok(name) = record_name("matt@mattcorallo.com") {
        let _ = read_proof(bytes, &name, 1_794_588_509);
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        let _ = (onchain(text), normalize(text));
    }
}

#[cfg(test)]
mod tests {
    use bahaa_types::hex;
    use dnssec_prover::rr::DnsKey;
    use serde_json::Value;

    use super::*;

    const NOW: u64 = 1_794_588_509;
    const PAID: &str = "bc1qztwy6xen3zdtt7z0vrgapmjtfz8acjkfp5fp7l";

    fn recorded(key: &str) -> (Name, Vec<u8>) {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/bip353.json")).unwrap();
        let entry = &fixture[key];
        (
            record_name(entry["name"].as_str().unwrap()).unwrap(),
            hex::decode(entry["proof"].as_str().unwrap()).unwrap(),
        )
    }

    fn paid() -> Address {
        Address::parse(ChainFamily::Bitcoin, PAID).unwrap()
    }

    #[test]
    fn names_are_lowercase_ascii_users_at_domains() {
        assert_eq!(
            normalize(" \u{20bf}Matt@MattCorallo.com").unwrap(),
            "\u{20bf}matt@mattcorallo.com"
        );
        assert!(normalize("simple@dnssec_proof_tests.bitcoin.ninja").is_ok());
        for bad in [
            "matt",
            "matt@com",
            "m\u{e4}tt@x.com",
            "a b@x.com",
            "@x.com",
            "a@x..com",
        ] {
            assert!(normalize(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn recorded_proofs_check_out_only_inside_their_signature_window() {
        let (asked, proof) = recorded("matt");
        assert_eq!(read_proof(&proof, &asked, NOW), Ok(Some(paid())));
        assert_eq!(read_proof(&proof, &asked, 1_900_000_000), Err(UNPROVEN));
        let (simple, simple_proof) = recorded("simple");
        assert_eq!(read_proof(&simple_proof, &simple, NOW), Ok(Some(paid())));
        assert_eq!(read_proof(&proof, &simple, NOW), Ok(None));
        let mut forged = proof;
        let last = forged.len() - 1;
        forged[last] ^= 1;
        assert!(read_proof(&forged, &asked, NOW).is_err());
    }

    #[test]
    fn payment_instructions_give_one_mainnet_address() {
        for uri in [
            format!("bitcoin:{PAID}?lno=lno1zr5q"),
            format!("bitcoin:?bc={PAID}"),
            format!("BITCOIN:?BC={}", PAID.to_ascii_uppercase()),
            format!("bitcoin:{PAID}?amount=1&label=x"),
        ] {
            assert_eq!(onchain(&uri).ok(), Some(paid()), "{uri}");
        }
        for uri in [
            "bitcoin:?lno=lno1zr5q".to_owned(),
            "bitcoin:?lightning=lnbc1".to_owned(),
            format!("bitcoin:{PAID}?req-pop=x"),
            "bitcoin:?bc=1BoatSLRHtKNngkdXEeobR76b53LETtpyT".to_owned(),
            "bitcoin:tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx".to_owned(),
            format!("bitcoin:x{PAID}"),
            format!("lightning:{PAID}"),
        ] {
            assert!(onchain(&uri).is_err(), "{uri}");
        }
    }

    #[test]
    fn rsa_keys_under_1024_bits_are_too_weak() {
        let key = |alg: u8, pubkey: Vec<u8>| {
            RR::DnsKey(DnsKey {
                name: Name::try_from("com.").unwrap(),
                flags: 257,
                protocol: 3,
                alg,
                pubkey,
            })
        };
        let modulus = |bytes: usize, top: u8| {
            let mut key = vec![3, 1, 0, 1];
            key.push(top);
            key.extend(vec![0xab; bytes - 1]);
            key
        };
        assert!(!weak(&key(8, modulus(128, 0x80))));
        assert!(weak(&key(8, modulus(128, 0x7f))));
        assert!(weak(&key(10, modulus(64, 0xff))));
        let mut long_exponent = vec![0, 0, 3, 1, 0, 1];
        long_exponent.extend(modulus(256, 0xc0)[4..].to_vec());
        assert!(!weak(&key(8, long_exponent)));
        assert!(!weak(&key(13, vec![1; 64])));
        assert!(weak(&key(8, vec![9])));
    }
}
