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
            self.assertEqual(json.loads((output / 'notarization-result.json').read_text())['status'], 'Invalid')

    def test_accepted_notarization_staples_and_validates(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder)
            response = package.subprocess.CompletedProcess([], 0, json.dumps({'status':'Accepted'}), '')
            with patch.object(package.subprocess, 'run', return_value=response), patch.object(package, 'run') as run:
                package.notarize(output / 'app.dmg', ['key','id','issuer'], output)
                self.assertEqual([call.args[:3] for call in run.call_args_list], [('xcrun','stapler','staple'), ('xcrun','stapler','validate')])

if __name__ == '__main__':
    unittest.main()
