"""Verify lookup and invalidation of explicit NSH command hashes."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


NSH = str(Path(sys.argv.pop(1)).resolve(strict=True))


class HashTests(unittest.TestCase):
    def setUp(self):
        sandbox = tempfile.TemporaryDirectory(prefix="nsh-hash-")
        self.addCleanup(sandbox.cleanup)
        self.root = Path(sandbox.name)
        self.executable("first/pending", "first")
        self.executable("second/probe", "second")
        self.executable("nested/first/probe", "nested")

    def executable(self, name, label):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"echo {label}\n", encoding="ascii")
        path.chmod(0o755)

    def run_shell(self, code, output):
        result = subprocess.run(
            [NSH, "--posix", "-c", code],
            cwd=self.root,
            env={"PATH": "/usr/bin:/bin", "LC_ALL": "C"},
            check=False,
            capture_output=True,
            timeout=5,
        )
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, output, b""))

    def test_explicit_hash_prioritizes_then_falls_back_to_path_order(self):
        self.run_shell(
            """PATH=first:second:/usr/bin
hash probe
/usr/bin/mv first/pending first/probe
probe
command -v probe
hash probe
probe
/usr/bin/rm first/probe
probe
PATH="$PATH"
hash
""",
            b"second\nsecond/probe\nfirst\nsecond\n",
        )

    def test_temporary_path_keeps_table_and_command_p_has_builtin_effect(self):
        self.run_shell(
            """PATH=second:/usr/bin
hash probe
PATH=first:second hash probe
hash
command -p hash -r
hash
""",
            b"second/probe\n",
        )

    def test_relative_search_is_refreshed_after_changing_directory(self):
        self.run_shell(
            """PATH=first:second:/usr/bin
hash probe
cd nested
hash
probe
""",
            b"nested\n",
        )


if __name__ == "__main__":
    unittest.main()
