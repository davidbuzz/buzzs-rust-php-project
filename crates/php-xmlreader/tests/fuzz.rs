//! Randomised `XMLReader` call sequences: no panics, no memory errors.

#![allow(clippy::cast_possible_wrap)]

use php_libxml::tree::Document;
use php_xmlreader::{CollectingHost, IssetKind, Property, XmlReader};

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % (n as u64)).unwrap()
    }
}

const DOCS: &[&str] = &[
    "<r a=\"1\" xmlns:x=\"urn:x\" x:b=\"2\"><x:i>t</x:i><!--c--><?p d?><![CDATA[z]]><e/></r>",
    "<?xml version=\"1.0\"?><!DOCTYPE r [<!ENTITY e \"v\">]><r>&e;<a xml:lang=\"en\">x</a></r>",
    "<r><unclosed></r>",
    "not xml at all",
];

#[test]
fn random_reader_calls() {
    php_libxml::error::use_internal_errors(Some(true));
    let base =
        Document::parse_memory(b"<base/>", &php_libxml::tree::ParseOptions::default()).unwrap();
    for seed in 1..=60u64 {
        let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut h = CollectingHost::default();
        let mut x = XmlReader::new();
        let mut kept = Vec::new();
        for _ in 0..200 {
            match r.below(16) {
                0 => {
                    let flags = [0, 2, 4, 1 << 8][r.below(4)];
                    let _ = x.xml(DOCS[r.below(DOCS.len())].as_bytes(), None, flags, &mut h);
                }
                1..=4 => {
                    let _ = x.read();
                }
                5 => {
                    let _ = x.next([None, Some(&b"a"[..])][r.below(2)]);
                }
                6 => {
                    let _ = x.move_to_first_attribute();
                    let _ = x.move_to_next_attribute();
                    let _ = x.move_to_attribute_no(r.below(3) as i64 - 1);
                }
                7 => {
                    let _ = x.move_to_element();
                    let _ = x.get_attribute(b"a");
                    let _ = x.get_attribute_ns(b"b", b"urn:x");
                    let _ = x.lookup_namespace(b"x");
                }
                8 => {
                    let _ = x.read_inner_xml();
                    let _ = x.read_outer_xml();
                    let _ = x.read_string();
                }
                9 => {
                    if let Ok(Some(n)) = x.expand([None, Some(&base)][r.below(2)], &mut h) {
                        kept.push(n);
                    }
                }
                10 => {
                    for p in Property::ALL {
                        let _ = x.read_property(p);
                        let _ = x.has_property(p, IssetKind::NotEmpty);
                    }
                    let _ = x.debug_properties();
                }
                11 => {
                    let _ = x.set_parser_property(r.below(6) as i64, r.below(2) == 0);
                    let _ = x.get_parser_property(r.below(6) as i64);
                }
                12 => {
                    let _ = x.set_relaxng_schema_source(
                        Some(br#"<element name="r" xmlns="http://relaxng.org/ns/structure/1.0"><text/></element>"#),
                        &mut h,
                    );
                }
                13 => {
                    let _ = x.is_valid();
                }
                14 => {
                    x.close();
                }
                _ => {
                    kept.clear();
                }
            }
        }
    }
    php_libxml::error::reset_request_state();
}
