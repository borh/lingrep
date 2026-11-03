import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'perf_measure.py'
spec = importlib.util.spec_from_file_location('perf_measure', SCRIPT)
measure = importlib.util.module_from_spec(spec)
spec.loader.exec_module(measure)


class NativeMeasurementTests(unittest.TestCase):
    def test_calibration_block_count_cannot_be_extended_or_screened(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'campaign.json').write_text(json.dumps({'manifest': {
                'escalation_ladder': [6], 'calibration_wall_tolerance_pct': 2}}))
            (root / 'measure.py').write_bytes(SCRIPT.read_bytes())
            (root / 'host.json').write_text(json.dumps({'measurement_backend': 'gnu-time'}))
            (root / 'gnu-time').touch()
            (root / 'gnu-time').chmod(0o755)
            for flags in (['--blocks', '8'], ['--screen']):
                result = subprocess.run(['bash', str(SCRIPT.with_name('perf_campaign.sh')),
                    'run', '--dir', str(root), '--catalog', 'patterns/double-wo.lx', *flags],
                    capture_output=True, text=True)
                self.assertEqual(result.returncode, 2)
                self.assertIn('calibration block count is fixed', result.stderr)

    def test_gnu_rss_matches_native_resource_units(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            raw = root / 'raw-rss'
            child = ('import resource; from pathlib import Path; '
                     'data=bytearray(64 * 1024 * 1024); '
                     'Path(' + repr(str(raw)) + ').write_text(str(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss))')
            receipt = root / 'receipt.json'
            self.assertEqual(measure.measure([sys.executable, '-c', child], receipt), 0)
            native = float(raw.read_text()) * (1 if sys.platform == 'darwin' else 1024)
            recorded = json.loads(receipt.read_text())['peak_rss_bytes']
            self.assertLess(abs(recorded - native), 8 * 1024 * 1024)

    def test_timing_excludes_run_state_probes_and_preserves_exit(self):
        def observe():
            time.sleep(0.15)
            return {'system': sys.platform == 'darwin' and 'Darwin' or 'Linux'}
        with tempfile.TemporaryDirectory() as directory, patch.object(measure, 'run_state', observe):
            receipt = Path(directory) / 'receipt.json'
            start = time.monotonic()
            code = measure.measure([sys.executable, '-c', 'raise SystemExit(7)'], receipt)
            total = time.monotonic() - start
            row = json.loads(receipt.read_text())
            self.assertEqual(code, 7)
            self.assertEqual(row['exit'], 7)
            self.assertLess(row['wall_seconds'], total - 0.25)
            self.assertGreater(row['peak_rss_bytes'], 0)
            self.assertEqual(row['memory_capacity'], 'not qualified')

    def test_peak_rss_is_measured_per_child(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(measure, 'run_state', return_value={
                'system': 'Darwin' if sys.platform == 'darwin' else 'Linux'}):
            root = Path(directory)
            measure.measure([sys.executable, '-c', 'data = bytearray(64 * 1024 * 1024)'], root / 'large.json')
            measure.measure([sys.executable, '-c', 'pass'], root / 'small.json')
            large = json.loads((root / 'large.json').read_text())
            small = json.loads((root / 'small.json').read_text())
            self.assertLess(small['peak_rss_bytes'], large['peak_rss_bytes'])

    def test_interruption_kills_child_and_retains_receipt(self):
        for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            with self.subTest(signal=signum):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    (root / 'measure.py').write_bytes(SCRIPT.read_bytes())
                    pidfile = root / 'pid'
                    receipt = root / 'receipt.json'
                    child = 'import os,time; from pathlib import Path; Path(%r).write_text(str(os.getpid())); time.sleep(60)' % str(pidfile)
                    process = subprocess.Popen([sys.executable, str(SCRIPT), 'run', '--output', str(receipt),
                                                '--', sys.executable, '-c', child])
                    try:
                        deadline = time.monotonic() + 20
                        while not pidfile.exists() and process.poll() is None and time.monotonic() < deadline:
                            time.sleep(0.02)
                        self.assertTrue(pidfile.exists(), 'measurement child did not start')
                        process.send_signal(signum)
                        self.assertEqual(process.wait(timeout=20), 128 + signum)
                        self.assertIn('KeyboardInterrupt' if signum == signal.SIGINT else 'InterruptedError',
                                      json.loads(receipt.read_text())['error'])
                        with self.assertRaises(ProcessLookupError):
                            os.kill(int(pidfile.read_text()), 0)
                    finally:
                        if process.poll() is None:
                            process.kill()
                            process.wait()
                    if pidfile.exists():
                        try:
                            os.kill(int(pidfile.read_text()), signal.SIGKILL)
                        except ProcessLookupError:
                            pass

    def test_exec_failure_retains_diagnostic(self):
        with tempfile.TemporaryDirectory() as directory:
            receipt = Path(directory) / 'receipt.json'
            self.assertEqual(measure.measure(['/does-not-exist/ling'], receipt), 127)
            self.assertEqual(json.loads(receipt.read_text())['exit'], 127)
            self.assertIn('status 127', receipt.with_suffix('.time.txt').read_text())

    def test_darwin_reports_unavailable_controls(self):
        with patch.object(measure.platform, 'system', return_value='Darwin'), \
                patch.object(measure, 'probe', return_value={'exit': 1, 'stderr': 'unavailable'}):
            row = measure.host()
        self.assertFalse(row['affinity']['supported'])
        self.assertIn('not qualified', row['memory_capacity'])
        self.assertEqual(row['observations']['xctrace']['exit'], 1)

    def test_campaign_records_native_fields_and_removes_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'measure.py').write_bytes(SCRIPT.read_bytes())
            recorder = root / 'recorder'
            recorder.write_text('#!' + sys.executable + '\nimport json,sys\nfrom pathlib import Path\n'
                                + 'Path(' + repr(str(root / 'record.json')) + ').write_text(json.dumps(sys.argv[1:]))\n')
            recorder.chmod(0o755)
            shell = r"""
PERF_CAMPAIGN_LIB_ONLY=1 source "$1"
tool=$2
ARM_A=$3
AFFINITY=''
WORKERS=1
STREAM_OUTPUT=0
LINGREP_DICTIONARY_PATH=unused
CAMPAIGN_ARGV=(-c 'print("measurement output")')
run_one "$4" patterns/test.lx a 0 0 ''
"""
            subprocess.run(['bash', '-c', shell, 'test', str(SCRIPT.with_name('perf_campaign.sh')),
                            str(recorder), sys.executable, str(root)], check=True)
            args = json.loads((root / 'record.json').read_text())
            rss = float(args[args.index('--peak-rss-kb') + 1])
            receipt = json.loads(next((root / 'measurements').glob('*.json')).read_text())
            self.assertEqual(rss * 1024, receipt['peak_rss_bytes'])
            self.assertEqual(float(args[args.index('--wall') + 1]), receipt['wall_seconds'])
            self.assertFalse(list(root.glob('.run.*')))
            self.assertTrue(list((root / 'measurements').glob('*.stderr')))

    def test_streaming_requires_producer_and_consumer_success(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(measure, 'run_state', return_value={
                'system': 'Darwin' if sys.platform == 'darwin' else 'Linux'}):
            for producer, consumer in [(0, 0), (7, 0), (0, 9)]:
                with self.subTest(producer=producer, consumer=consumer):
                    receipt = Path(directory) / f'{producer}-{consumer}.json'
                    code = measure.measure(
                        [sys.executable, '-c', f'print("output"); raise SystemExit({producer})'], receipt,
                        [sys.executable, '-c', f'import sys; sys.stdin.read(); sys.exit({consumer})'])
                    self.assertEqual(code, producer or consumer)
                    self.assertEqual(json.loads(receipt.read_text())['consumer_exit'], consumer)

    def test_interrupting_runner_cleans_measurement_and_output(self):
        for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            with self.subTest(signal=signum):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    (root / 'measure.py').write_bytes(SCRIPT.read_bytes())
                    pidfile = root / 'pid'
                    child = 'import os,time; from pathlib import Path; Path(%r).write_text(str(os.getpid())); time.sleep(60)' % str(pidfile)
                    shell = r"""
        PERF_CAMPAIGN_LIB_ONLY=1 source "$1"
        ARM_A=$2
        AFFINITY=''
        WORKERS=1
        STREAM_OUTPUT=0
        CAMPAIGN_ARGV=(-c "$4")
        run_one "$3" patterns/test.lx a 0 0 ''
        """
                    process = subprocess.Popen(['bash', '-c', shell, 'test', str(SCRIPT.with_name('perf_campaign.sh')),
                                                sys.executable, str(root), child])
                    try:
                        deadline = time.monotonic() + 20
                        while not pidfile.exists() and process.poll() is None and time.monotonic() < deadline:
                            time.sleep(0.02)
                        self.assertTrue(pidfile.exists())
                        process.send_signal(signum)
                        self.assertEqual(process.wait(timeout=20), 128 + signum)
                        self.assertFalse(list(root.glob('.run.*')))
                        receipt = next((root / 'measurements').glob('*.json'))
                        self.assertIn('InterruptedError', json.loads(receipt.read_text())['error'])
                        with self.assertRaises(ProcessLookupError):
                            os.kill(int(pidfile.read_text()), 0)
                    finally:
                        if process.poll() is None:
                            process.kill()
                            process.wait()
                    if pidfile.exists():
                        try:
                            os.kill(int(pidfile.read_text()), signal.SIGKILL)
                        except ProcessLookupError:
                            pass

    def test_failed_baseline_build_removes_owned_worktree(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo = root / 'repo'
            (repo / 'scripts').mkdir(parents=True)
            (repo / 'scripts/perf_measure.py').write_bytes(SCRIPT.read_bytes())
            (repo / 'input').write_text('test')
            subprocess.run(['git', 'init', '-q', str(repo)], check=True)
            subprocess.run(['git', '-C', str(repo), 'add', '.'], check=True)
            subprocess.run(['git', '-C', str(repo), '-c', 'user.name=Test', '-c', 'user.email=test@localhost',
                            'commit', '-qm', 'test fixture'], check=True)
            shell = r"""
PERF_CAMPAIGN_LIB_ONLY=1 source "$1"
repo_root=$2
cd "$repo_root"
build_tool() { :; }
freeze_arm() { printf '%s\n' /usr/bin/true; }
cargo() { case "$PWD" in */.baseline-worktree) return 9;; *) return 0;; esac; }
cmd_new --dir "$3" --baseline HEAD --input "$repo_root/input" --affinity ''
"""
            result = subprocess.run(['bash', '-c', shell, 'test', str(SCRIPT.with_name('perf_campaign.sh')),
                                     str(repo), str(root / 'campaign')], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((root / 'campaign/.baseline-worktree').exists(), result.stderr)
            trees = subprocess.check_output(['git', '-C', str(repo), 'worktree', 'list', '--porcelain'], text=True)
            self.assertEqual(trees.count('worktree '), 1)

    def test_reaped_process_group_is_not_signaled(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(measure.os, 'killpg') as killpg:
            self.assertEqual(measure.measure([sys.executable, '-c', 'pass'], Path(directory) / 'r.json'), 0)
            killpg.assert_not_called()

    def test_all_platform_metrics_come_from_gnu_time(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            timer = root / 'time'
            timer.write_text('#!' + sys.executable + '\nimport subprocess,sys\nfrom pathlib import Path\n'
                             'assert sys.argv[1] == "-o" and sys.argv[3] == "-f"\n'
                             'result=subprocess.run(sys.argv[5:])\n'
                             'Path(sys.argv[2]).write_text("1.25 2.5 0.75 1234\\n")\n'
                             'sys.exit(result.returncode)\n')
            timer.chmod(0o755)
            receipt = root / 'r.json'
            for system in ('Linux', 'Darwin'):
                with self.subTest(system=system), patch.object(measure, 'run_state', return_value={'system': system}):
                    receipt = root / (system + '.json')
                    self.assertEqual(measure.measure([sys.executable, '-c', 'pass'], receipt, gnu_time=timer), 0)
                    row = json.loads(receipt.read_text())
                    self.assertEqual((row['wall_seconds'], row['user_seconds'], row['system_seconds'], row['peak_rss_bytes']),
                                     (1.25, 2.5, 0.75, 1234 * 1024))
                    self.assertEqual(row['clock'], 'GNU time')

    def test_memory_snapshot_preserves_scope_units_and_probe_failures(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(measure.platform, 'system', return_value='Darwin'), \
                patch.object(measure, 'host', return_value={'observations': {'memory': 'host VM counters'}}):
            output = Path(directory) / 'memory'
            def collect(argv):
                if argv[0] == 'footprint':
                    (output / 'footprint.json').write_text(json.dumps({'unit': 'byte', 'bytes per unit': 1,
                        'processes': [{'pid': 12, 'footprint': 4000,
                                       'categories': {'mapped file': {'clean': 2000, 'swapped': 300}}}]}))
                    return {'exit': 0}
                return {'exit': 1, 'stderr': 'permission denied'}
            with patch.object(measure, 'probe', side_effect=collect):
                measure.memory_snapshot(12, output)
            row = json.loads((output / 'snapshot.json').read_text())
            self.assertEqual(row['physical_footprint_bytes'], 4000)
            self.assertEqual(row['footprint_categories_bytes']['mapped file']['clean'], 2000)
            self.assertIn('combines', row['process_compression_and_swap'])
            self.assertEqual(row['vmmap']['exit'], 1)
            self.assertEqual(row['memory_capacity'], 'not qualified')
            with self.assertRaises(FileExistsError):
                measure.memory_snapshot(12, output)


class WarmupCampaignTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        estimator = os.environ.get('LINGREP_PERF_TEST_BIN')
        if not estimator or not Path(estimator).is_file() or not os.access(estimator, os.X_OK):
            raise RuntimeError('lingrep-perf estimator missing: build it and set LINGREP_PERF_TEST_BIN to its executable path')
        cls.estimator = str(Path(estimator).resolve())

    def campaign(self, root, slow=False, fail_on=0, warmups=1):
        (root / 'warmups' / 'other.lx.warmup.previous').mkdir(parents=True)
        (root / 'measure.py').write_bytes(SCRIPT.read_bytes())
        (root / 'host.json').write_text(json.dumps({'measurement_backend': 'gnu-time'}))
        (root / 'campaign.json').write_text(json.dumps({'manifest': {
            'warmup_runs_per_arm': warmups, 'arms': {arm: {'binary_digest': 'blake3:test'} for arm in ('a', 'b')}}}))
        target = root / 'target.py'
        target.write_text('import os,sys,time\nfrom pathlib import Path\n'
            'states=Path(sys.argv[1]); states.mkdir(exist_ok=True); state=states/os.environ["TEST_ARM"]\n'
            'n=int(state.read_text()) if state.exists() else 0; state.write_text(str(n+1))\n'
            'calls=states.parent/"calls"; invocation=(int(calls.read_text()) if calls.exists() else 0)+1\n'
            'calls.write_text(str(invocation))\n'
            'if n == 0: time.sleep(float(sys.argv[2]))\n'
            'print("{}")\n'
            'raise SystemExit(7 if invocation == int(os.environ.get("FAIL_ON",0)) else 0)\n')
        for arm in ('a', 'b'):
            launcher = root / ('arm-' + arm)
            launcher.write_text('#!' + sys.executable + '\nimport os,runpy,sys\n'
                + 'os.environ["TEST_ARM"]=' + repr('arm-' + arm) + '\n'
                + 'sys.argv=sys.argv[1:]; runpy.run_path(sys.argv[0],run_name="__main__")\n')
            launcher.chmod(0o755)
        timer = root / 'gnu-time'
        timer.write_text('#!' + sys.executable + '\nimport subprocess,sys\nfrom pathlib import Path\n'
            'state=Path(sys.argv[-2])/Path(sys.argv[5]).name; cold=not state.exists() and float(sys.argv[-1]) > 0\n'
            'result=subprocess.run(sys.argv[5:])\n'
            'Path(sys.argv[2]).write_text(f"{9 if cold else 1} 1 0 1024\\n")\n'
            'raise SystemExit(result.returncode)\n')
        timer.chmod(0o755)
        tool = root / 'tool'
        tool.write_text('#!' + sys.executable + '\nimport json,sys\nfrom pathlib import Path\n'
            'root=Path(__file__).parent; records=root/"records.json"\n'
            'rows=json.loads(records.read_text()) if records.exists() else []\n'
            'cmd=sys.argv[1]\n'
            'if cmd == "digest": print("blake3:test")\n'
            'elif cmd == "template":\n'
            ' print(root/"target.py"); print(root/"states"); print(' + repr('0.05' if slow else '0') + ')\n'
            'elif cmd == "plan":\n'
            ' n=len(rows); print(f"patterns/test.lx\\tblocks\\t3\\t{n//4}\\t{n%4}\\t{n//4}\\t{n%4}\\t4\\t-")\n'
            'elif cmd == "record":\n'
            ' def arg(name): return sys.argv[sys.argv.index(name)+1]\n'
            ' rows.append({"arm":arg("--arm"),"block":int(arg("--block")),"wall":float(arg("--wall"))})\n'
            ' records.write_text(json.dumps(rows))\n'
            ' if (root/"interrupt").exists() and len(rows)==3:\n'
            '  (root/"interrupt").unlink(); raise SystemExit(1)\n')
        tool.chmod(0o755)
        (root / 'workload.env').write_text(f'ARM_A={root / "arm-a"}\nARM_B={root / "arm-b"}\n'
            'AFFINITY=\nWORKERS=1\nSTREAM_OUTPUT=0\nDIGEST_AFFINITY=\n')
        return dict(os.environ, LINGREP_DICTIONARY_PATH='test-dictionary', FAIL_ON=str(fail_on))

    def run_campaign(self, root, env):
        shell = 'PERF_CAMPAIGN_LIB_ONLY=1 source "$1"; build_tool() { :; }; tool="$2/tool"; cmd_run --dir "$2" --catalog patterns/test.lx --blocks 3'
        return subprocess.run(['bash', '-eu', '-o', 'pipefail', '-c', shell, 'test',
            str(SCRIPT.with_name('perf_campaign.sh')), str(root)], env=env, capture_output=True, text=True)

    def test_slow_first_execution_is_excluded_from_fixed_block_estimate(self):
        results = []
        for slow in (False, True):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                result = self.run_campaign(root, self.campaign(root, slow=slow, warmups=1))
                self.assertEqual(result.returncode, 0, result.stderr)
                rows = json.loads((root / 'records.json').read_text())
                self.assertEqual(len(rows), 12)
                for arm in ('a', 'b'):
                    with self.subTest(slow=slow, arm=arm, check='measured arm is warm'):
                        self.assertEqual([row['wall'] for row in rows if row['arm'] == arm], [1] * 6)
                warmups = [json.loads(p.read_text()) for p in (root / 'warmups').glob('*/*.json')]
                for arm in ('a', 'b'):
                    with self.subTest(slow=slow, arm=arm, check='warm-up receipt per arm'):
                        self.assertEqual(sum(row['arm'] == arm for row in warmups), 1)
                self.assertEqual({row['phase'] for row in warmups}, {'warmup'})
                samples = [[row['wall'] for arm in ('a', 'b') for row in rows
                            if row['block'] == block and row['arm'] == arm] for block in range(3)]
                path = root / 'samples.json'
                path.write_text(json.dumps(samples))
                result = subprocess.run([self.estimator, 'estimate', str(path)], check=True, capture_output=True, text=True)
                results.append(json.loads(result.stdout))
        with self.subTest(check='same estimate and interval'):
            self.assertEqual(results[0], results[1])
        with self.subTest(check='no cold cost in estimate'):
            self.assertEqual((results[1]['ratio'], results[1]['low'], results[1]['high']), (1, 1, 1))

    def test_nondefault_warmup_count_applies_to_each_arm(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = self.run_campaign(root, self.campaign(root, slow=True, warmups=2))
            self.assertEqual(result.returncode, 0, result.stderr)
            receipts = [json.loads(p.read_text()) for p in (root / 'warmups').glob('*/*.json')]
            for arm in ('a', 'b'):
                with self.subTest(arm=arm):
                    self.assertEqual(sum(row['arm'] == arm for row in receipts), 2)

    def test_failed_warmup_prevents_measurements_and_resume_repeats_both_arms(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            env = self.campaign(root, fail_on=2)
            result = self.run_campaign(root, env)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((root / 'records.json').exists())
            receipts = [json.loads(p.read_text()) for p in (root / 'warmups').glob('*/*.json')]
            self.assertEqual([row['exit'] for row in receipts].count(7), 1)
            env['FAIL_ON'] = '0'
            (root / 'interrupt').touch()
            result = self.run_campaign(root, env)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(len(json.loads((root / 'records.json').read_text())), 3)
            result = self.run_campaign(root, env)
            self.assertEqual(result.returncode, 0, result.stderr)
            receipts = [json.loads(p.read_text()) for p in (root / 'warmups').glob('*/*.json')]
            self.assertEqual(sum(row['phase'] == 'resumed-warmup' for row in receipts), 4)
            self.assertEqual(len(json.loads((root / 'records.json').read_text())), 12)
            count = len(receipts)
            self.assertEqual(self.run_campaign(root, env).returncode, 0)
            self.assertEqual(len(list((root / 'warmups').glob('*/*.json'))), count)
