//! The native extensions linked into the `php` binary.
//!
//! Binding crates (`crates/php-ext-*`) add one line here (and a dependency
//! in `crates/php-cli/Cargo.toml`); see `crates/php-vm/EXTENSIONS.md`.

use php_vm::ext::Extension;

/// All linked extensions, in registration order.
pub fn all() -> Vec<Box<dyn Extension>> {
    vec![
        Box::new(php_ext_stdstr::StdStr),
        Box::new(php_ext_streams::Streams),
        Box::new(php_ext_date::Date),
        // Box::new(php_ext_ctype::Ctype),
        Box::new(php_ext_json::Json),
        Box::new(php_ext_hash::Hash),
        Box::new(php_ext_random::Random),
        Box::new(php_ext_filter::Filter),
        Box::new(php_ext_pcre::Pcre),
        Box::new(php_ext_stdarr::StdArray),
        Box::new(php_ext_stdmisc::StdMisc),
        Box::new(php_ext_spl::Spl),
        Box::new(php_ext_mbstring::Mbstring),
        Box::new(php_ext_pdo::Pdo),
        Box::new(php_ext_session::Session),
        Box::new(php_ext_openssl::OpenSsl),
        Box::new(php_ext_sodium::Sodium),
        Box::new(php_ext_fileinfo::Fileinfo),
        Box::new(php_ext_posix::PosixExt),
        Box::new(php_ext_iconv::IconvExt),
        Box::new(php_ext_intl::Intl),
        Box::new(php_ext_exif::Exif),
        Box::new(php_ext_uri::Uri),
        Box::new(php_ext_zlib::Zlib),
        Box::new(php_ext_zip::Zip),
        Box::new(php_ext_phar::Phar),
        Box::new(php_ext_gettext::Gettext),
        Box::new(php_ext_sysv::Sysvmsg),
        Box::new(php_ext_sysv::Sysvsem),
        Box::new(php_ext_sysv::Sysvshm),
        Box::new(php_ext_sysv::Shmop),
        Box::new(php_ext_pcntl::PcntlExt),
        Box::new(php_ext_sockets::SocketsExt),
        Box::new(php_ext_calendar::Calendar),
        Box::new(php_ext_bcmath::Bcmath),
        Box::new(php_ext_gmp::Gmp),
        Box::new(php_ext_xml::Xml),
        Box::new(php_ext_xmlreader::XmlReaderExt),
        Box::new(php_ext_simplexml::SimpleXml),
    ]
}
