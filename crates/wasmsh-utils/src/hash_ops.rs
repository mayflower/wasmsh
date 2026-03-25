//! Hash utilities: sha1sum, sha512sum.

use crate::helpers::{hashsum_util, hex_encode};
use crate::UtilContext;

// ---------------------------------------------------------------------------
// SHA-1 -- clean-room implementation of RFC 3174
// ---------------------------------------------------------------------------

#[allow(clippy::many_single_char_names)]
fn sha1_digest(data: &[u8]) -> [u8; 20] {
    let mut h0: u32 = 0x6745_2301;
    let mut h1: u32 = 0xEFCD_AB89;
    let mut h2: u32 = 0x98BA_DCFE;
    let mut h3: u32 = 0x1032_5476;
    let mut h4: u32 = 0xC3D2_E1F0;

    // Pre-processing: pad to 64-byte blocks (big-endian length)
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    // Process each 64-byte (512-bit) block
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in w.iter_mut().enumerate().take(16) {
            let base = i * 4;
            *word = u32::from_be_bytes([
                chunk[base],
                chunk[base + 1],
                chunk[base + 2],
                chunk[base + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (h0, h1, h2, h3, h4);

        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDCu32),
                _ => (b ^ c ^ d, 0xCA62_C1D6u32),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut result = [0u8; 20];
    result[0..4].copy_from_slice(&h0.to_be_bytes());
    result[4..8].copy_from_slice(&h1.to_be_bytes());
    result[8..12].copy_from_slice(&h2.to_be_bytes());
    result[12..16].copy_from_slice(&h3.to_be_bytes());
    result[16..20].copy_from_slice(&h4.to_be_bytes());
    result
}

pub(crate) fn util_sha1sum(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    hashsum_util(ctx, argv, "sha1sum", |data| hex_encode(&sha1_digest(data)))
}

// ---------------------------------------------------------------------------
// SHA-512 -- clean-room implementation of FIPS 180-4
// ---------------------------------------------------------------------------

const SHA512_K: [u64; 80] = [
    0x428a_2f98_d728_ae22,
    0x7137_4491_23ef_65cd,
    0xb5c0_fbcf_ec4d_3b2f,
    0xe9b5_dba5_8189_dbbc,
    0x3956_c25b_f348_b538,
    0x59f1_11f1_b605_d019,
    0x923f_82a4_af19_4f9b,
    0xab1c_5ed5_da6d_8118,
    0xd807_aa98_a303_0242,
    0x1283_5b01_4570_6fbe,
    0x2431_85be_4ee4_b28c,
    0x550c_7dc3_d5ff_b4e2,
    0x72be_5d74_f27b_896f,
    0x80de_b1fe_3b16_96b1,
    0x9bdc_06a7_25c7_1235,
    0xc19b_f174_cf69_2694,
    0xe49b_69c1_9ef1_4ad2,
    0xefbe_4786_384f_25e3,
    0x0fc1_9dc6_8b8c_d5b5,
    0x240c_a1cc_77ac_9c65,
    0x2de9_2c6f_592b_0275,
    0x4a74_84aa_6ea6_e483,
    0x5cb0_a9dc_bd41_fbd4,
    0x76f9_88da_8311_53b5,
    0x983e_5152_ee66_dfab,
    0xa831_c66d_2db4_3210,
    0xb003_27c8_98fb_213f,
    0xbf59_7fc7_beef_0ee4,
    0xc6e0_0bf3_3da8_8fc2,
    0xd5a7_9147_930a_a725,
    0x06ca_6351_e003_826f,
    0x1429_2967_0a0e_6e70,
    0x27b7_0a85_46d2_2ffc,
    0x2e1b_2138_5c26_c926,
    0x4d2c_6dfc_5ac4_2aed,
    0x5338_0d13_9d95_b3df,
    0x650a_7354_8baf_63de,
    0x766a_0abb_3c77_b2a8,
    0x81c2_c92e_47ed_aee6,
    0x9272_2c85_1482_353b,
    0xa2bf_e8a1_4cf1_0364,
    0xa81a_664b_bc42_3001,
    0xc24b_8b70_d0f8_9791,
    0xc76c_51a3_0654_be30,
    0xd192_e819_d6ef_5218,
    0xd699_0624_5565_a910,
    0xf40e_3585_5771_202a,
    0x106a_a070_32bb_d1b8,
    0x19a4_c116_b8d2_d0c8,
    0x1e37_6c08_5141_ab53,
    0x2748_774c_df8e_eb99,
    0x34b0_bcb5_e19b_48a8,
    0x391c_0cb3_c5c9_5a63,
    0x4ed8_aa4a_e341_8acb,
    0x5b9c_ca4f_7763_e373,
    0x682e_6ff3_d6b2_b8a3,
    0x748f_82ee_5def_b2fc,
    0x78a5_636f_4317_2f60,
    0x84c8_7814_a1f0_ab72,
    0x8cc7_0208_1a64_39ec,
    0x90be_fffa_2363_1e28,
    0xa450_6ceb_de82_bde9,
    0xbef9_a3f7_b2c6_7915,
    0xc671_78f2_e372_532b,
    0xca27_3ece_ea26_619c,
    0xd186_b8c7_21c0_c207,
    0xeada_7dd6_cde0_eb1e,
    0xf57d_4f7f_ee6e_d178,
    0x06f0_67aa_7217_6fba,
    0x0a63_7dc5_a2c8_98a6,
    0x113f_9804_bef9_0dae,
    0x1b71_0b35_131c_471b,
    0x28db_77f5_2304_7d84,
    0x32ca_ab7b_40c7_2493,
    0x3c9e_be0a_15c9_bebc,
    0x431d_67c4_9c10_0d4c,
    0x4cc5_d4be_cb3e_42b6,
    0x597f_299c_fc65_7e2a,
    0x5fcb_6fab_3ad6_faec,
    0x6c44_198c_4a47_5817,
];

const SHA512_H: [u64; 8] = [
    0x6a09_e667_f3bc_c908,
    0xbb67_ae85_84ca_a73b,
    0x3c6e_f372_fe94_f82b,
    0xa54f_f53a_5f1d_36f1,
    0x510e_527f_ade6_82d1,
    0x9b05_688c_2b3e_6c1f,
    0x1f83_d9ab_fb41_bd6b,
    0x5be0_cd19_137e_2179,
];

#[allow(clippy::many_single_char_names)]
fn sha512_digest(data: &[u8]) -> [u8; 64] {
    let mut h = SHA512_H;

    // Pre-processing: pad to 128-byte blocks (big-endian 128-bit length)
    let bit_len = (data.len() as u128).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 128 != 112 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    // Process each 128-byte (1024-bit) block
    for chunk in msg.chunks_exact(128) {
        let mut w = [0u64; 80];
        for (i, word) in w.iter_mut().enumerate().take(16) {
            let base = i * 8;
            *word = u64::from_be_bytes([
                chunk[base],
                chunk[base + 1],
                chunk[base + 2],
                chunk[base + 3],
                chunk[base + 4],
                chunk[base + 5],
                chunk[base + 6],
                chunk[base + 7],
            ]);
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;

        for i in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA512_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut result = [0u8; 64];
    for (i, val) in h.iter().enumerate() {
        result[i * 8..(i + 1) * 8].copy_from_slice(&val.to_be_bytes());
    }
    result
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
                stdin,
                state: None,
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
