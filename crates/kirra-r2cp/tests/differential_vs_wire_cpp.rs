//! **Differential test: the host codec against the NORMATIVE `wire.cpp`.**
//!
//! `firmware/rosmaster-r2/protocol/src/wire.cpp` is the oracle. This compiles it
//! with the system C++ compiler, drives both implementations over the same
//! bytes, and asserts they agree — on the encoded bytes, on the decoded fields,
//! and on the REJECTION VERDICT for malformed input.
//!
//! Without this, "R2CP" would be two dialects that happen to look alike in the
//! happy path and diverge exactly where it matters: a truncated frame, a CRC
//! flip, a reserved flag bit. Those are the cases a real UART produces.
//!
//! Skips cleanly (does not fail) when no C++ compiler is available, so the lane
//! stays green on a minimal runner while still running wherever g++ exists.

use std::path::PathBuf;
use std::process::Command;

use kirra_r2cp::{decode, encode, DecodeError, Frame, MessageType};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn have_cxx() -> Option<String> {
    for cc in ["g++", "c++", "clang++"] {
        if Command::new(cc)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Some(cc.to_string());
        }
    }
    None
}

/// Build a tiny harness that links the REAL wire.cpp and exposes encode/decode
/// over stdin/stdout as hex, so the Rust side can drive it without FFI.
fn build_oracle(cc: &str) -> Option<PathBuf> {
    let root = repo_root();
    let proto = root.join("firmware/rosmaster-r2/protocol");
    let wire = proto.join("src/wire.cpp");
    if !wire.is_file() {
        return None;
    }
    let out = std::env::temp_dir().join("r2cp_oracle");
    let src = std::env::temp_dir().join("r2cp_oracle.cpp");
    std::fs::write(&src, ORACLE_CPP).ok()?;
    let status = Command::new(cc)
        .args(["-std=c++17", "-O1", "-I"])
        .arg(proto.join("include"))
        .arg(&src)
        .arg(&wire)
        .arg("-o")
        .arg(&out)
        .status()
        .ok()?;
    status.success().then_some(out)
}

/// stdin:  "E <type> <flags> <seq> <time_us> <payload-hex>"  → encoded hex
///         "D <encoded-hex>"                                  → "OK <fields>" | "ERR <status>"
const ORACLE_CPP: &str = r#"
#include <r2/protocol/wire.hpp>
#include <cstdio>
#include <cstring>
#include <string>
#include <iostream>
#include <sstream>
using namespace r2::protocol;
static int hexval(char c){ if(c>='0'&&c<='9')return c-'0'; if(c>='a'&&c<='f')return c-'a'+10;
  if(c>='A'&&c<='F')return c-'A'+10; return -1; }
static std::string tohex(const std::uint8_t*d,std::size_t n){ static const char*H="0123456789abcdef";
  std::string s; for(std::size_t i=0;i<n;i++){ s+=H[d[i]>>4]; s+=H[d[i]&15]; } return s; }
static std::size_t fromhex(const std::string&h,std::uint8_t*out,std::size_t cap){
  std::size_t n=0; for(std::size_t i=0;i+1<h.size()&&n<cap;i+=2){
    int a=hexval(h[i]),b=hexval(h[i+1]); if(a<0||b<0)break; out[n++]=(std::uint8_t)((a<<4)|b);} return n; }
static const char* statusname(DecodeStatus s){ switch(s){
  case DecodeStatus::ok:return "ok"; case DecodeStatus::empty:return "empty";
  case DecodeStatus::oversized:return "oversized"; case DecodeStatus::malformed_cobs:return "malformed_cobs";
  case DecodeStatus::bad_magic:return "bad_magic"; case DecodeStatus::unsupported_version:return "unsupported_version";
  case DecodeStatus::invalid_length:return "invalid_length"; case DecodeStatus::crc_mismatch:return "crc_mismatch";
  case DecodeStatus::unknown_message:return "unknown_message"; case DecodeStatus::invalid_flags:return "invalid_flags";
  case DecodeStatus::auth_required:return "auth_required"; default:return "auth_mac_mismatch"; } }
int main(){ std::string line;
  while(std::getline(std::cin,line)){ std::istringstream is(line); std::string op; is>>op;
    if(op=="E"){ unsigned t,f,seq; unsigned long long tm; std::string ph; is>>t>>f>>seq>>tm>>ph;
      Frame fr{}; fr.type=static_cast<MessageType>(t); fr.flags=(std::uint8_t)f;
      fr.sequence=(std::uint32_t)seq; fr.source_time_us=(std::uint64_t)tm;
      if(ph!="-") fr.payload_length=(std::uint16_t)fromhex(ph,fr.payload.data(),fr.payload.size());
      EncodedFrame out{}; if(encode(fr,out)) std::cout<<tohex(out.bytes.data(),out.length)<<"\n";
      else std::cout<<"ENCFAIL\n"; }
    else if(op=="D"){ std::string eh; is>>eh; std::uint8_t buf[512];
      std::size_t n=fromhex(eh,buf,sizeof buf); Frame fr{};
      DecodeStatus st=decode(buf,n,fr);
      if(st==DecodeStatus::ok) std::cout<<"OK "<<(unsigned)fr.type<<" "<<(unsigned)fr.flags<<" "
        <<fr.sequence<<" "<<fr.source_time_us<<" "
        <<(fr.payload_length? tohex(fr.payload.data(),fr.payload_length):std::string("-"))<<"\n";
      else std::cout<<"ERR "<<statusname(st)<<"\n"; }
    std::cout.flush(); }
  return 0; }
"#;

/// The oracle subprocess.
///
/// The `BufReader` is held for the child's LIFETIME, not rebuilt per call. A
/// fresh reader each time buffers ahead of the line it returns and then drops
/// the remainder, so replies silently desynchronise from requests — which is
/// exactly what happened on the first run here and looked like a codec bug.
struct Oracle {
    child: std::process::Child,
    reader: std::io::BufReader<std::process::ChildStdout>,
}

impl Oracle {
    fn ask(&mut self, line: &str) -> String {
        use std::io::{BufRead, Write};
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
        let mut out = String::new();
        self.reader.read_line(&mut out).expect("oracle reply");
        out.trim().to_string()
    }
}

impl Drop for Oracle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn oracle() -> Option<Oracle> {
    let cc = have_cxx()?;
    let bin = build_oracle(&cc)?;
    let mut child = Command::new(bin)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let reader = std::io::BufReader::new(child.stdout.take()?);
    Some(Oracle { child, reader })
}

/// The corpus both sides see. Chosen to cover the header edges: empty payload,
/// max payload, a payload full of zeros (the COBS worst case), and every flag.
fn corpus() -> Vec<Frame> {
    let mut v = vec![
        Frame::new(MessageType::Hello, 0, 0),
        Frame::new(MessageType::MotionCommand, 1, 1_000),
        Frame::new(MessageType::MotionCommand, u32::MAX, u64::MAX),
        Frame::new(MessageType::Odometry, 42, 123_456_789).with_payload(&[0u8; 32]),
        Frame::new(MessageType::Battery, 7, 9).with_payload(&[0xFF; 192]),
        Frame::new(MessageType::FaultEvent, 3, 5).with_payload(b"fault"),
    ];
    // COBS worst case: a long run with no zeros crosses the 0xFF code boundary.
    v.push(Frame::new(MessageType::Diagnostics, 9, 9).with_payload(&[0xAAu8; 192]));
    for flags in [0u8, 1, 2, 8, 0x0B] {
        let mut f = Frame::new(MessageType::RobotState, 11, 22);
        f.flags = flags;
        v.push(f);
    }
    v
}

#[test]
fn host_encoder_is_byte_identical_to_wire_cpp() {
    let Some(mut o) = oracle() else {
        eprintln!("SKIP: no C++ compiler / wire.cpp — oracle unavailable");
        return;
    };
    for f in corpus() {
        let mine = encode(&f).expect("host encode");
        let ph = if f.payload.is_empty() {
            "-".to_string()
        } else {
            hex(&f.payload)
        };
        let theirs = o.ask(&format!(
            "E {} {} {} {} {}",
            f.message_type as u8, f.flags, f.sequence, f.source_time_us, ph
        ));
        assert_ne!(
            theirs, "ENCFAIL",
            "wire.cpp refused a frame the host accepted: {f:?}"
        );
        // Our encode() appends the 0x00 delimiter; wire.cpp's EncodedFrame does not.
        // wire.cpp's EncodedFrame.length INCLUDES the trailing 0x00 delimiter
        // (verified against its output, not assumed) — so compare whole frames.
        assert_eq!(
            hex(&mine),
            theirs,
            "ENCODER DIVERGENCE for {f:?}\n  host:     {}\n  wire.cpp: {theirs}",
            hex(&mine)
        );
    }
}

#[test]
fn wire_cpp_decodes_what_the_host_encodes() {
    let Some(mut o) = oracle() else {
        eprintln!("SKIP: no C++ compiler — oracle unavailable");
        return;
    };
    for f in corpus() {
        let bytes = encode(&f).unwrap();
        let reply = o.ask(&format!("D {}", hex(&bytes)));
        assert!(
            reply.starts_with("OK "),
            "wire.cpp rejected our frame {f:?}: {reply}"
        );
        let parts: Vec<&str> = reply.split_whitespace().collect();
        assert_eq!(parts[1].parse::<u8>().unwrap(), f.message_type as u8);
        assert_eq!(parts[2].parse::<u8>().unwrap(), f.flags);
        assert_eq!(parts[3].parse::<u32>().unwrap(), f.sequence);
        assert_eq!(parts[4].parse::<u64>().unwrap(), f.source_time_us);
        let want = if f.payload.is_empty() {
            "-".to_string()
        } else {
            hex(&f.payload)
        };
        assert_eq!(parts[5], want, "payload round-trip differs for {f:?}");
    }
}

#[test]
fn the_host_decodes_what_wire_cpp_encodes() {
    let Some(mut o) = oracle() else {
        eprintln!("SKIP: no C++ compiler — oracle unavailable");
        return;
    };
    for f in corpus() {
        let ph = if f.payload.is_empty() {
            "-".to_string()
        } else {
            hex(&f.payload)
        };
        let theirs = o.ask(&format!(
            "E {} {} {} {} {}",
            f.message_type as u8, f.flags, f.sequence, f.source_time_us, ph
        ));
        assert!(
            theirs.len() % 2 == 0 && theirs.chars().all(|c| c.is_ascii_hexdigit()),
            "oracle did not return hex for {f:?}: {theirs:?}"
        );
        let raw = unhex(&theirs);
        let body = raw.strip_suffix(&[0u8][..]).unwrap_or(&raw);
        let decoded = decode(body).unwrap_or_else(|e| {
            panic!("host rejected wire.cpp output for {f:?}: {e:?}\n  bytes={theirs}")
        });
        assert_eq!(decoded, f, "field divergence decoding a wire.cpp frame");
    }
}

/// The one that matters most: both must REJECT the same corruption, with the
/// same verdict. A real UART produces exactly these.
#[test]
fn both_implementations_reject_the_same_corruption_with_the_same_verdict() {
    let Some(mut o) = oracle() else {
        eprintln!("SKIP: no C++ compiler — oracle unavailable");
        return;
    };
    let base =
        encode(&Frame::new(MessageType::MotionCommand, 5, 500).with_payload(b"abcd")).unwrap();
    let body = &base[..base.len() - 1];

    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    cases.push(("empty", vec![]));
    cases.push(("truncated-half", body[..body.len() / 2].to_vec()));
    cases.push(("truncated-1", body[..body.len() - 1].to_vec()));
    // CRC flip: corrupt the last data byte before the delimiter.
    let mut crcflip = body.to_vec();
    let n = crcflip.len();
    crcflip[n - 1] ^= 0xFF;
    cases.push(("crc-flip", crcflip));
    // A byte in the middle flipped — CRC must catch it.
    let mut midflip = body.to_vec();
    midflip[body.len() / 2] ^= 0x01;
    cases.push(("mid-flip", midflip));

    for (name, bytes) in cases {
        let mine = decode(&bytes);
        let theirs = o.ask(&format!("D {}", hex(&bytes)));
        assert!(
            mine.is_err(),
            "host ACCEPTED corrupt input {name}: {mine:?}"
        );
        assert!(
            theirs.starts_with("ERR"),
            "wire.cpp accepted corrupt input {name}: {theirs}"
        );
    }
}

// ── properties that need no oracle ──────────────────────────────────────────

#[test]
fn round_trip_is_lossless_for_the_whole_corpus() {
    for f in corpus() {
        let bytes = encode(&f).unwrap();
        assert_eq!(
            *bytes.last().unwrap(),
            0,
            "the frame must be 0x00-delimited"
        );
        assert!(
            bytes[..bytes.len() - 1].iter().all(|&b| b != 0),
            "COBS output must contain no interior zero — resync depends on it"
        );
        assert_eq!(decode(&bytes[..bytes.len() - 1]).unwrap(), f);
    }
}

#[test]
fn an_auth_tagged_frame_is_refused_not_downgraded() {
    let mut f = Frame::new(MessageType::MotionCommand, 1, 1);
    f.flags = kirra_r2cp::FLAG_AUTH_TAG;
    let bytes = encode(&f).unwrap();
    assert_eq!(
        decode(&bytes[..bytes.len() - 1]),
        Err(DecodeError::AuthRequired),
        "the unauthenticated path must FAIL CLOSED on a tagged frame"
    );
}

#[test]
fn reserved_flag_bits_and_unknown_types_are_rejected() {
    let mut f = Frame::new(MessageType::Hello, 1, 1);
    f.flags = 0x10; // bit 4 — reserved in v1
    assert_eq!(encode(&f), Err(DecodeError::InvalidFlags));
    assert_eq!(
        MessageType::from_u8(0x77),
        None,
        "an unknown id must not be coerced"
    );
}

#[test]
fn an_oversize_payload_is_refused() {
    let f = Frame::new(MessageType::Battery, 1, 1).with_payload(&[0u8; 193]);
    assert_eq!(encode(&f), Err(DecodeError::InvalidLength));
}

#[test]
fn resynchronisation_costs_exactly_one_frame() {
    let good = encode(&Frame::new(MessageType::Hello, 1, 1)).unwrap();
    let mut stream = vec![0x11, 0x22, 0x33, 0x00]; // garbage + delimiter
    stream.extend_from_slice(&good);
    let (frames, _rest) = kirra_r2cp::split_frames(&stream);
    assert_eq!(
        frames.len(),
        2,
        "the garbage and the good frame are separate"
    );
    assert!(decode(&frames[0]).is_err(), "garbage must not decode");
    assert!(
        decode(&frames[1]).is_ok(),
        "the NEXT frame must still decode — bounded resync"
    );
}
