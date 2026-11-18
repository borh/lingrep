import pathlib
import shutil
import subprocess
import tempfile
import unittest


GUARD = pathlib.Path(__file__).resolve().parents[1] / "check-fs-capability-seam.sh"
ALLOWLISTED = (
    "lingrep-corpus/src/fs_capability.rs",
    "lingrep-core/src/dictionary.rs",
    "lingrep-perf/src/campaign.rs",
    "lingrep/src/packaged_dictionaries.rs",
)


def run_guard(files, remove=()):
    """Run the guard over a tree holding the allowlisted files plus `files`."""
    with tempfile.TemporaryDirectory() as temporary:
        root = pathlib.Path(temporary)
        (root / "scripts").mkdir()
        shutil.copyfile(GUARD, root / "scripts" / GUARD.name)
        for path in ALLOWLISTED:
            if path not in remove:
                (root / path).parent.mkdir(parents=True, exist_ok=True)
                (root / path).touch()
        for path, text in files.items():
            (root / path).parent.mkdir(parents=True, exist_ok=True)
            (root / path).write_text(text)
        return subprocess.run(
            ["bash", str(root / "scripts" / GUARD.name)], capture_output=True, text=True
        )


class FsCapabilitySeamGuard(unittest.TestCase):
    def test_reports_each_direct_use_outside_the_seam(self):
        cases = {
            "fs::rename": 'use std::fs;\nfn f() { fs::rename("a", "b").unwrap(); }\n',
            "std::fs::rename": 'fn f() { std::fs::rename("a", "b").unwrap(); }\n',
            "glob-imported rename": 'use std::fs::*;\nfn f() { rename("a", "b").unwrap(); }\n',
            "braced import": 'use std::fs::{File, rename};\nfn f() { rename("a", "b").unwrap(); }\n',
            "aliased import": 'use std::fs::rename as mv;\nfn f() { mv("a", "b").unwrap(); }\n',
            "aliased module": 'use std::fs as files;\nfn f() { files::rename("a", "b").unwrap(); }\n',
            "nested import": 'use std::{fs::{self, hard_link}, io};\n',
            "hard_link": 'fn f() { std::fs::hard_link("a", "b").unwrap(); }\n',
            "symlink": 'fn f() { std::os::unix::fs::symlink("a", "b").unwrap(); }\n',
            "imported symlink": 'use std::os::unix::fs::symlink;\nfn f() { symlink("a", "b").unwrap(); }\n',
            "glob-imported unix fs": 'use std::os::unix::fs::*;\n',
            "persist": 'fn f(t: tempfile::NamedTempFile) { t.persist("b").unwrap(); }\n',
            "persist_noclobber": 'fn f(t: tempfile::NamedTempFile) { t.persist_noclobber("b").unwrap(); }\n',
            "renameat2": 'fn f() { unsafe { libc::renameat2(0, a, 0, b, 0) }; }\n',
            "renamex_np": 'fn f() { unsafe { renamex_np(a, b, 0) }; }\n',
            "unquoted WAL": 'const P: &str = "PRAGMA journal_mode = WAL";\n',
            "single-quoted WAL": 'const P: &str = "PRAGMA journal_mode = \'WAL\'";\n',
            "double-quoted WAL": 'const P: &str = "PRAGMA journal_mode = \\"WAL\\"";\n',
            "locking_mode": 'const P: &str = "PRAGMA locking_mode = EXCLUSIVE";\n',
        }
        for label, text in cases.items():
            with self.subTest(label=label):
                result = run_guard({"lingrep/src/lib.rs": text})
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("lingrep/src/lib.rs:", result.stderr)

    def test_accepts_exempt_forms(self):
        cases = {
            "comments": {"lingrep/src/lib.rs": "// fs::rename(a, b)\n/* std::fs::hard_link(a, b) */\n"},
            "non-SQL string": {"lingrep/src/lib.rs": 'const S: &str = "fs::rename(a, b) persist(x)";\n'},
            "unrelated imports": {
                "lingrep/src/lib.rs": "use std::fs::{self, File};\nuse std::os::unix::fs::PermissionsExt;\n"
            },
            "inline test module": {
                "lingrep/src/lib.rs": '#[cfg(test)]\nmod tests {\n    use std::fs::*;\n'
                '    fn f() { rename("a", "b").unwrap(); }\n}\n'
            },
            "test module file": {
                "lingrep/src/lib.rs": "#[cfg(test)]\nmod tests;\n",
                "lingrep/src/tests.rs": 'fn f() { std::fs::rename("a", "b").unwrap(); }\n',
            },
            "#[path] test file": {
                "lingrep/src/lib.rs": '#[cfg(test)]\n#[path = "lib_tests.rs"]\nmod tests;\n',
                "lingrep/src/lib_tests.rs": 'fn f() { std::fs::hard_link("a", "b").unwrap(); }\n',
            },
            "allowlisted entry": {
                "lingrep-core/src/dictionary.rs": 'fn f() { std::fs::rename("a", "b").unwrap(); }\n'
            },
        }
        for label, files in cases.items():
            with self.subTest(label=label):
                result = run_guard(files)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_rejects_a_stale_allowlist_path(self):
        result = run_guard({}, remove={"lingrep-perf/src/campaign.rs"})
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("lingrep-perf/src/campaign.rs", result.stderr)


if __name__ == "__main__":
    unittest.main()
