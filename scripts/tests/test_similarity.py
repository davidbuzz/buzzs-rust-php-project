# buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
# Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
"""Tests of scripts/similarity.py: what is a token, what is left out, and the figure of two small
files worked out by hand. Run: python3 -m unittest scripts/tests/test_similarity.py"""
import math
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import similarity as sim  # noqa: E402


class Tokens(unittest.TestCase):
    def test_c_comments_strings_and_numbers_leave_no_tokens(self):
        src = """
        /* commented_name in a block */
        // line_comment_name
        static int helper_count = 0x20ac; /* 0x20ac must not leave "x20ac" */
        const char *text = "string_name \\" still_string";
        char quote = '\\'';
        double big = 1e10f;
        """
        self.assertEqual(sim.tokens(src, "c", set()), {"static", "helper_count", "const", "char", "text", "quote", "double"})

    def test_rust_raw_strings_chars_and_lifetimes(self):
        src = """
        // line_comment_name
        /* block_comment_name */
        fn parse_header<'life>(input: &'life str) -> usize {
            let raw = r#"raw_string_name "quoted" more"#;
            let ch = '\\n'; let q = '"'; let plain = "plain_string_name";
            input.len() + 0xff_usize
        }
        """
        got = sim.tokens(src, "rust", set())
        self.assertIn("parse_header", got)
        self.assertIn("input", got)
        self.assertIn("life", got)
        for gone in ("line_comment_name", "block_comment_name", "raw_string_name", "quoted", "plain_string_name", "xff_usize", "ff_usize"):
            self.assertNotIn(gone, got)

    def test_engine_prefixes_short_names_and_the_dropped_vocabulary(self):
        drop = {"strlen", "needle"} | sim.C_KEYWORDS
        got = sim.tokens("zend_hash_find(ht, key); php_strlen(needle); int len; ZSTR_VAL(str); _zend_new_array_0();", "c", drop)
        # `zend_hash_find` -> `hash_find`; `php_strlen` -> `strlen`, which a PHP program sees;
        # `needle` is a stub parameter; `ht`, `key`, `len`, `str`, `int` and `val` are short or keywords.
        self.assertEqual(got, {"hash_find", "new_array_0"})

    def test_stub_names_are_what_a_php_program_sees(self):
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "ext").mkdir()
            (root / "ext" / "x.stub.php").write_text(
                "<?php\n/** @generate-class-entries */\n"
                "/** @var int\n * @cvalue PHP_X_INTERNAL_VALUE */\nconst X_PUBLIC = UNKNOWN;\n"
                "function x_open(string $filename, int $mode_flags = 0): X|false {}\n"
                "final class XHandle { public const int LIMIT = 3; public function readChunk(int $length): string {} }\n"
            )
            names = sim.stub_names(root)
        for n in ("x_open", "filename", "mode_flags", "xhandle", "readchunk", "length", "x_public", "limit", "php_x_internal_value"):
            self.assertIn(n, names)


class Figure(unittest.TestCase):
    def corpus(self):
        """A corpus of two C files, built by hand: weights ln((2 + 1) / (df + 1))."""
        c1 = {"alpha_walk", "beta_slot", "gamma_node", "delta_mark", "omega_flag", "shared_everywhere"}
        c2 = {"other_thing", "shared_everywhere"}
        corpus = sim.Corpus.__new__(sim.Corpus)
        corpus.min_shared = 5
        corpus.drop = set()
        corpus.cset = {"a.c": c1, "b.c": c2}
        df = {}
        for s in (c1, c2):
            for t in s:
                df[t] = df.get(t, 0) + 1
        corpus.df_c = df
        corpus.idf = {t: math.log(3 / (k + 1)) for t, k in df.items()}
        corpus.cnorm = {c: math.sqrt(sum(corpus.idf[t] ** 2 for t in s)) for c, s in corpus.cset.items()}
        corpus.cweight = {c: sum(corpus.idf[t] for t in s) for c, s in corpus.cset.items()}
        corpus.postings = None
        return corpus

    def test_the_figure_of_two_small_files_by_hand(self):
        corpus = self.corpus()
        w = math.log(3 / 2)  # a name of one C file
        self.assertAlmostEqual(corpus.idf["shared_everywhere"], 0.0)  # in every C file: no weight
        self.assertAlmostEqual(corpus.idf["alpha_walk"], w)
        # The Rust file uses the five names of a.c and one name of b.c.
        r = {"alpha_walk", "beta_slot", "gamma_node", "delta_mark", "omega_flag", "other_thing", "shared_everywhere"}
        cos, a, b, shared = corpus.pair("a.c", r)
        self.assertEqual(len(shared), 6)
        self.assertAlmostEqual(cos, 5 / math.sqrt(30))  # 5w^2 / (sqrt(5)w * sqrt(6)w)
        self.assertAlmostEqual(a, 1.0)
        self.assertAlmostEqual(b, 5 / 6)
        # Too few shared names: no figure is given for such a pair.
        self.assertLess(len(corpus.pair("b.c", r)[3]), corpus.min_shared)
        # The nearest file is a.c, and it is the only one with a figure.
        near = corpus.nearest(r)
        self.assertEqual([c for *_, c in near], ["a.c"])
        self.assertAlmostEqual(near[0][0], 5 / math.sqrt(30))

    def test_the_rule_ranks_source_files_only(self):
        corpus = self.corpus()
        # A header that repeats the names of a.c is nearer than a.c: the rule passes it over.
        names = {"alpha_walk", "beta_slot", "gamma_node", "delta_mark", "omega_flag"}
        w = math.log(3 / 2)
        corpus.cset["a.h"] = set(names)
        corpus.cnorm["a.h"] = math.sqrt(5 * w * w)
        corpus.cweight["a.h"] = 5 * w
        # ... and a.c has a name of its own, which the Rust file does not use.
        corpus.cset["a.c"] = corpus.cset["a.c"] | {"zeta_private"}
        corpus.idf["zeta_private"] = w
        corpus.cnorm["a.c"] = math.sqrt(6 * w * w)
        corpus.cweight["a.c"] = 6 * w
        r = names | {"other_thing"}
        self.assertEqual(corpus.nearest(r, top=1)[0][4], "a.h")
        self.assertEqual(corpus.nearest(r, top=1, sources_only=True)[0][4], "a.c")

    def test_names_the_rust_file_invented_count_for_nothing(self):
        import tempfile

        corpus = self.corpus()
        with tempfile.NamedTemporaryFile("w", suffix=".rs", delete=False) as f:
            f.write("fn alpha_walk(beta_slot: u8) { let invented_here = gamma_node + delta_mark + omega_flag; }\n")
            path = f.name
        try:
            rs = corpus.rust_tokens(path)
        finally:
            Path(path).unlink()
        self.assertEqual(rs, {"alpha_walk", "beta_slot", "gamma_node", "delta_mark", "omega_flag"})
        cos, a, b, _ = corpus.pair("a.c", rs)
        self.assertAlmostEqual(cos, 1.0)
        self.assertAlmostEqual(a, 1.0)
        self.assertAlmostEqual(b, 1.0)


if __name__ == "__main__":
    unittest.main()
