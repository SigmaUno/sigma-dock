#!/usr/bin/env python3
"""Distribution policy tests without Apple credentials or native signing tools."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import package_macos as package

class DistributionPolicy(unittest.TestCase):
    def test_test_build_is_explicitly_supported(self):
        package.distribution_policy(False, '', [None, None, None])

    def test_production_rejects_missing_or_wrong_identity_and_bundle_only(self):
        for identity, keys, app_only in [('', [None]*3, False),
                ('Apple Development: Example', ['key','id','issuer'], False),
                ('Developer ID Application: Example', ['key','id',None], False),
                ('Developer ID Application: Example', ['key','id','issuer'], True)]:
            with self.assertRaises(ValueError):
                package.distribution_policy(True, identity, keys, app_only)
        package.distribution_policy(True, 'Developer ID Application: Example', ['key','id','issuer'])

    def test_rejected_notarization_never_staples_even_with_zero_exit_code(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder)
            response = package.subprocess.CompletedProcess([], 0, json.dumps({'status':'Invalid'}), '')
            with patch.object(package.subprocess, 'run', return_value=response), patch.object(package, 'run') as run:
                with self.assertRaises(SystemExit):
                    package.notarize(output / 'app.dmg', ['key','id','issuer'], output)
                run.assert_not_called()
            self.assertEqual(json.loads((output / 'notarization-dmg-result.json').read_text())['status'], 'Invalid')

    def test_accepted_notarization_staples_and_validates(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder)
            response = package.subprocess.CompletedProcess([], 0, json.dumps({'status':'Accepted'}), '')
            with patch.object(package.subprocess, 'run', return_value=response), patch.object(package, 'run') as run:
                package.notarize(output / 'app.dmg', ['key','id','issuer'], output)
                self.assertEqual([call.args[:3] for call in run.call_args_list], [('xcrun','stapler','staple'), ('xcrun','stapler','validate')])

class SignaturePolicy(unittest.TestCase):
    identity = 'Developer ID Application: Example (TEAMID)'

    def test_exact_authority_and_code_directory_runtime_required(self):
        valid = 'CodeDirectory v=20500 size=100 flags=0x10000(runtime) hashes=1\nAuthority=' + self.identity + '\n'
        package.verify_signature(valid, self.identity)
        for signature in [valid.replace('(runtime)', '(none)'),
                          valid.replace(self.identity, self.identity + ' impostor'),
                          valid.replace('CodeDirectory', 'Other runtime metadata')]:
            with self.assertRaises(SystemExit):
                package.verify_signature(signature, self.identity)


class AppTicket(unittest.TestCase):
    def test_app_submission_staples_app_and_keeps_separate_diagnostics(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder)
            response = package.subprocess.CompletedProcess([], 0, json.dumps({'status': 'Accepted'}), '')
            with patch.object(package.subprocess, 'run', return_value=response) as submit, patch.object(package, 'run') as run:
                package.notarize(output / 'app.zip', ['key', 'id', 'issuer'], output,
                                 ticket_target=output / 'SigmaDock.app', label='app')
                self.assertEqual(submit.call_args.args[0][3], str(output / 'app.zip'))
                self.assertEqual([call.args for call in run.call_args_list], [
                    ('xcrun', 'stapler', 'staple', str(output / 'SigmaDock.app')),
                    ('xcrun', 'stapler', 'validate', str(output / 'SigmaDock.app'))])
            self.assertTrue((output / 'notarization-app-result.json').is_file())
            self.assertFalse((output / 'notarization-dmg-result.json').exists())

    def test_ticket_failure_stops_distribution(self):
        with tempfile.TemporaryDirectory() as folder:
            response = package.subprocess.CompletedProcess([], 0, json.dumps({'status': 'Accepted'}), '')
            with patch.object(package.subprocess, 'run', return_value=response), patch.object(package, 'run',
                    side_effect=package.subprocess.CalledProcessError(1, ['stapler'])):
                with self.assertRaises(package.subprocess.CalledProcessError):
                    package.notarize(Path(folder) / 'app.dmg', ['key', 'id', 'issuer'], Path(folder))

    def test_invalid_apple_response_never_staples(self):
        with tempfile.TemporaryDirectory() as folder:
            response = package.subprocess.CompletedProcess([], 1, 'not JSON', '')
            with patch.object(package.subprocess, 'run', return_value=response), patch.object(package, 'run') as run:
                with self.assertRaises(SystemExit):
                    package.notarize(Path(folder) / 'app.dmg', ['key', 'id', 'issuer'], Path(folder))
                run.assert_not_called()

class PackagingOrder(unittest.TestCase):
    def exercise(self, reject_app=False):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            binaries = root / 'binaries'
            binaries.mkdir()
            for name in package.BINARIES:
                (binaries / name).write_bytes(b'test fixture')
            output = root / 'dist'
            calls = []
            def command(*args):
                calls.append(args)
                if args[:2] == ('hdiutil', 'create'):
                    Path(args[-1]).write_bytes(b'disk image fixture')
            def accepted(artifact, keys, destination, **kwargs):
                calls.append(('notarize', kwargs.get('label', 'dmg')))
                if reject_app:
                    raise SystemExit('App rejected')
            argv = ['package_macos.py', '--production', '--bin-dir', str(binaries),
                    '--output', str(output), '--version', '0.1.3', '--build-id', 'abcdef012345', '--arch', 'arm64']
            environment = {'APPLE_SIGNING_IDENTITY': 'Developer ID Application: Example',
                           'APPLE_NOTARY_KEY_PATH': 'key', 'APPLE_API_KEY_ID': 'id', 'APPLE_API_ISSUER': 'issuer'}
            def inspect(args, **kwargs):
                return 'arm64' if args[0] == 'lipo' else ''
            with patch.object(package.sys, 'argv', argv), patch.object(package.sys, 'platform', 'darwin'), \
                    patch.dict(package.os.environ, environment), patch.object(package, 'run', side_effect=command), \
                    patch.object(package.subprocess, 'check_output', side_effect=inspect), \
                    patch.object(package, 'notarize', side_effect=accepted), \
                    patch.object(package, 'verify_distribution', side_effect=lambda *args: calls.append(('verify',))), \
                    patch('builtins.print'):
                if reject_app:
                    with self.assertRaises(SystemExit):
                        package.main()
                    self.assertFalse(list(output.glob('*.dmg')))
                    self.assertFalse(list(output.glob('*.sha256')))
                    self.assertFalse(any(call[0] == 'hdiutil' for call in calls))
                else:
                    package.main()
                    self.assertLess(calls.index(('notarize', 'app')),
                                    next(i for i, call in enumerate(calls) if call[:2] == ('hdiutil', 'create')))
                    self.assertLess(calls.index(('notarize', 'dmg')), calls.index(('verify',)))
                    self.assertEqual(len(list(output.glob('*.sha256'))), 1)

    def test_app_acceptance_precedes_image_and_final_verification(self):
        self.exercise()

    def test_app_rejection_prevents_image_and_checksum(self):
        self.exercise(reject_app=True)

if __name__ == '__main__':
    unittest.main()
