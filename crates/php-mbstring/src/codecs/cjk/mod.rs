// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! CJK encodings (port of `libmbfl/filters/mbfilter_cjk.c`) and the
//! Japanese mobile UTF-8 variants (`mbfilter_utf8.c`).
//!
//! Each submodule owns the [`Encoding`](crate::encoding::Encoding) statics of
//! one encoding family; they are re-exported here.

pub mod big5;
pub mod common;
pub mod cp5022x;
pub mod euc;
pub mod eucjp;
pub mod gb18030;
pub mod hz;
pub mod iso2022kr;
pub mod jis;
pub mod jis2004;
pub mod kana;
pub mod sjis;
#[cfg(test)]
pub(crate) mod testutil;
pub mod utf8_mobile;

pub use big5::{ENC_BIG5, ENC_CP950};
pub use cp5022x::{ENC_CP50220, ENC_CP50221, ENC_CP50222};
pub use euc::{ENC_EUC_CN, ENC_EUC_KR, ENC_EUC_TW, ENC_UHC};
pub use eucjp::{ENC_CP51932, ENC_EUCJP_WIN, ENC_EUC_JP};
pub use gb18030::{ENC_CP936, ENC_GB18030, ENC_GB18030_2022};
pub use hz::ENC_HZ;
pub use iso2022kr::ENC_2022KR;
pub use jis::{ENC_2022JP, ENC_2022JPMS, ENC_2022JP_KDDI, ENC_JIS};
pub use jis2004::{ENC_2022JP_2004, ENC_EUCJP2004, ENC_SJIS2004};
pub use sjis::{
    ENC_CP932, ENC_SJIS, ENC_SJISWIN, ENC_SJIS_DOCOMO, ENC_SJIS_KDDI, ENC_SJIS_MAC, ENC_SJIS_SB,
};
pub use utf8_mobile::{ENC_UTF8_DOCOMO, ENC_UTF8_KDDI_A, ENC_UTF8_KDDI_B, ENC_UTF8_SB};

#[cfg(test)]
mod tests {
    use super::testutil;

    /// Mixed CJK text and error cases through every encoding of this module,
    /// checked against php 8.3 (`tests/fixtures/cjk/smoke_oracle.php`).
    #[test]
    fn smoke_oracle_all_encodings() {
        assert!(testutil::replay_oracle("smoke_oracle.tsv") > 600);
    }
}
