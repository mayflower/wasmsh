//! Hash utilities: sha1sum, sha512sum.
//!
//! Backed by the `sha1` and `sha2` crates from the `RustCrypto` project.
//! See ADR-0024, which supersedes ADR-0015's clean-room mandate.

use crate::helpers::{hashsum_util, hex_encode};
use crate::UtilContext;

fn sha1_digest(data: &[u8]) -> [u8; 20] {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(data);
    h.finalize().into()
}

pub(crate) fn util_sha1sum(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    hashsum_util(ctx, argv, "sha1sum", |data| hex_encode(&sha1_digest(data)))
}

fn sha512_digest(data: &[u8]) -> [u8; 64] {
    use sha2::{Digest, Sha512};
    let mut h = Sha512::new();
    h.update(data);
    h.finalize().into()
}

pub(crate) fn util_sha512sum(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    hashsum_util(ctx, argv, "sha512sum", |data| {
        hex_encode(&sha512_digest(data))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UtilContext, VecOutput};
    use wasmsh_fs::{MemoryFs, OpenOptions, Vfs};

    fn run_hash(
        name: &str,
        func: fn(&mut UtilContext<'_>, &[&str]) -> i32,
        argv: &[&str],
        fs: &mut MemoryFs,
        stdin: Option<&[u8]>,
    ) -> (i32, String) {
        let mut output = VecOutput::default();
        let status = {
            let mut ctx = UtilContext {
                fs,
                output: &mut output,
                cwd: "/",
                stdin: stdin.map(crate::UtilStdin::from_bytes),
                state: None,
                network: None,
            };
            func(&mut ctx, argv)
        };
        let _ = name;
        (status, output.stdout_str().to_string())
    }

    #[test]
    fn sha1_empty() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_hash("sha1sum", util_sha1sum, &["sha1sum"], &mut fs, Some(b""));
        assert_eq!(status, 0);
        // SHA-1 of empty input = da39a3ee5e6b4b0d3255bfef95601890afd80709
        assert!(out.starts_with("da39a3ee5e6b4b0d3255bfef95601890afd80709"));
    }

    #[test]
    fn sha1_abc() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_hash("sha1sum", util_sha1sum, &["sha1sum"], &mut fs, Some(b"abc"));
        assert_eq!(status, 0);
        // SHA-1("abc") = a9993e364706816aba3e25717850c26c9cd0d89d
        assert!(out.starts_with("a9993e364706816aba3e25717850c26c9cd0d89d"));
    }

    #[test]
    fn sha1_file() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/test.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"hello").unwrap();
        fs.close(h);
        let (status, out) = run_hash(
            "sha1sum",
            util_sha1sum,
            &["sha1sum", "/test.txt"],
            &mut fs,
            None,
        );
        assert_eq!(status, 0);
        // SHA-1("hello") = aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d
        assert!(out.starts_with("aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d"));
        assert!(out.contains("/test.txt"));
    }

    #[test]
    fn sha512_empty() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_hash(
            "sha512sum",
            util_sha512sum,
            &["sha512sum"],
            &mut fs,
            Some(b""),
        );
        assert_eq!(status, 0);
        // SHA-512 of empty input
        assert!(out.starts_with(
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce\
             47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        ));
    }

    #[test]
    fn sha512_abc() {
        let mut fs = MemoryFs::new();
        let (status, out) = run_hash(
            "sha512sum",
            util_sha512sum,
            &["sha512sum"],
            &mut fs,
            Some(b"abc"),
        );
        assert_eq!(status, 0);
        // SHA-512("abc")
        assert!(out.starts_with(
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        ));
    }

    #[test]
    fn sha1_missing_file() {
        let mut fs = MemoryFs::new();
        let (status, _) = run_hash(
            "sha1sum",
            util_sha1sum,
            &["sha1sum", "/nope.txt"],
            &mut fs,
            None,
        );
        assert_eq!(status, 1);
    }

    #[test]
    fn sha512_file() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/data.bin", OpenOptions::write()).unwrap();
        fs.write_file(h, b"test data").unwrap();
        fs.close(h);
        let (status, out) = run_hash(
            "sha512sum",
            util_sha512sum,
            &["sha512sum", "/data.bin"],
            &mut fs,
            None,
        );
        assert_eq!(status, 0);
        assert!(out.contains("/data.bin"));
        // Just verify it produces a 128-char hex hash
        let hash_part = out.split("  ").next().unwrap();
        assert_eq!(hash_part.len(), 128);
    }
}
