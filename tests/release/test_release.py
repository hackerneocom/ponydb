"""Release integrity, version policy, and interrupted-publication regression tests."""
import argparse
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import urllib.error
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts'))
import release

COMMIT = 'a' * 40
DIGEST = 'sha256:' + 'b' * 64
IMAGES = {'ghcr': 'ghcr.io/hackerneocom/ponydb', 'dockerhub': 'docker.io/example/ponydb'}


def assets(directory, version='0.1.0'):
    entries = []
    for target in release.TARGETS:
        binary = b'release executable for ' + target.encode()
        files = {release.executable(target): binary, 'README.md': b'install',
                 'LICENSE': b'license', 'examples/ponydb.toml': b'# example'}
        archive = directory / release.archive_name(version, target)
        prefix = f'ponydb-{version}-{target}/'
        if target.endswith('-msvc'):
            with zipfile.ZipFile(archive, 'w') as stream:
                for name, data in files.items():
                    stream.writestr(prefix + name, data)
        else:
            with tarfile.open(archive, 'w:gz') as stream:
                for name, data in files.items():
                    info = tarfile.TarInfo(prefix + name)
                    info.size = len(data)
                    stream.addfile(info, io.BytesIO(data))
        entries.append({'target': target, 'archive': archive.name, 'sha256': release.sha256(archive),
                        'binary_sha256': hashlib.sha256(binary).hexdigest()})
    metadata = {'version': version, 'tag': 'v' + version, 'commit': COMMIT, 'rust': release.RUST,
                'artifacts': entries, 'images': {**IMAGES, 'digest': DIGEST}}
    release.write_json(directory / 'release-metadata.json', metadata)
    (directory / 'SHA256SUMS').write_text(''.join(entry['sha256'] + '  ' + entry['archive'] + '\n'
                                               for entry in entries), encoding='utf-8')
    return metadata


class VersionTests(unittest.TestCase):
    def test_release_versions_sort_numerically_and_prereleases_precede_stable(self):
        versions = ['0.9.0', '0.10.0-alpha.2', '0.10.0-alpha.10', '0.10.0-beta.0',
                    '0.10.0-rc.1', '0.10.0', '1.0.0']
        self.assertEqual(sorted(reversed(versions), key=release.version_key), versions)

    def test_reject_ambiguous_versions_and_docker_incompatible_metadata(self):
        for version in ('v1.0.0', '01.0.0', '1.01.0', '1.0.0-rc.01', '1.0.0-dev',
                        '1.0.0+build', '1.0.0\n', '1.0', '../1.0.0'):
            with self.subTest(version=version), self.assertRaises(ValueError):
                release.version_key(version)

    def test_old_maintenance_release_cannot_move_stable_alias_backwards(self):
        self.assertFalse(release.alias_allowed('0.10.0', '0.9.10'))
        self.assertFalse(release.alias_allowed('1.2.10', '1.2.9'))
        self.assertTrue(release.alias_allowed('1.2.9', '1.2.10'))
        self.assertTrue(release.alias_allowed('1.2.10', '1.2.10'))
        with self.assertRaises(ValueError):
            release.alias_allowed('1.2.0', '1.3.0-rc.1')


class ReleaseLookupTests(unittest.TestCase):
    def test_finds_drafts_and_published_releases_on_later_pages(self):
        for draft in (True, False):
            state = {'tag_name': 'v0.1.0', 'draft': draft, 'assets': []}
            pages = [[{'tag_name': 'v9.9.9'}] * 100, [state]]
            responses = [io.BytesIO(json.dumps(page).encode()) for page in pages]
            with self.subTest(draft=draft), patch.dict(os.environ, {'GH_TOKEN': 'fixture-token'}), \
                    patch.object(release.urllib.request, 'urlopen', side_effect=responses) as api:
                self.assertEqual(release.release('v0.1.0'), state)
                self.assertEqual([call.args[0].full_url for call in api.call_args_list], [
                    f'https://api.github.com/repos/{release.REPOSITORY}/releases?per_page=100&page={page}'
                    for page in (1, 2)])

    def test_missing_release_differs_from_api_failure(self):
        with patch.dict(os.environ, {'GH_TOKEN': 'fixture-token'}):
            with patch.object(release.urllib.request, 'urlopen', return_value=io.BytesIO(b'[]')):
                self.assertIsNone(release.release('v0.1.0'))
            for status in (403, 404, 500):
                error = urllib.error.HTTPError('https://api.github.com', status, 'API failure', {}, None)
                with self.subTest(status=status), \
                        patch.object(release.urllib.request, 'urlopen', side_effect=error):
                    with self.assertRaises((ValueError, urllib.error.HTTPError)):
                        release.release('v0.1.0')


class PrepareTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        (self.root / 'Cargo.toml').write_text('[workspace.package]\nversion = "0.1.0"\n')
        (self.root / 'Cargo.lock').write_text('[[package]]\nname = "ponydb"\nversion = "0.1.0"\n')
        (self.root / 'CHANGELOG.md').write_text('# Changelog\n\n## 0.1.0\n\nFirst release.\n')
        self.git('init', '--quiet', '--initial-branch=main')
        self.git('config', 'user.name', 'Release Test')
        self.git('config', 'user.email', 'release-test@example.invalid')
        self.git('add', '.')
        self.git('commit', '--quiet', '-m', 'fixture')
        self.git('update-ref', 'refs/remotes/origin/main', 'HEAD')
        self.git('tag', 'v0.1.0')

    def git(self, *args):
        return subprocess.run(['git', *args], cwd=self.root, check=True,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout.decode().strip()

    def prepare(self, tag='v0.1.0', dry_run=False):
        original_capture = release.capture

        def capture(*args, **kwargs):
            if args[0] == 'cargo':
                return json.dumps({'packages': [{'name': 'ponydb', 'version': '0.1.0'}]})
            return original_capture(*args, **kwargs)

        with patch.object(release, 'ROOT', self.root), patch.object(release, 'capture', side_effect=capture), \
                patch.dict(os.environ, {'GITHUB_REPOSITORY': release.REPOSITORY,
                                        'GITHUB_REF': 'refs/tags/v0.1.0',
                                        'DOCKERHUB_NAMESPACE': 'example'}), \
                patch.object(release, 'release') as lookup, patch.object(release, 'output') as output:
            release.prepare(argparse.Namespace(tag=tag, dry_run=dry_run))
            lookup.assert_not_called()
            return output.call_args.kwargs

    def test_tag_identity_and_exact_commit_are_exported(self):
        result = self.prepare()
        self.assertEqual(result['commit'], self.git('rev-parse', 'HEAD'))
        self.assertFalse(result['dry_run'])
        self.assertEqual(result['version'], '0.1.0')
        self.assertEqual(set(json.loads(result['asset_names'])), release.checkpoint_names('0.1.0'))

    def test_tag_version_and_checkout_must_agree(self):
        with self.assertRaisesRegex(ValueError, 'Tag must be'):
            self.prepare(tag='v0.2.0')
        (self.root / 'extra').write_text('new commit')
        self.git('add', '.')
        self.git('commit', '--quiet', '-m', 'different checkout')
        with self.assertRaisesRegex(ValueError, 'Checkout does not match'):
            self.prepare()

    def test_commit_outside_main_history_cannot_publish(self):
        (self.root / 'extra').write_text('not merged')
        self.git('add', '.')
        self.git('commit', '--quiet', '-m', 'not merged')
        self.git('tag', '--force', 'v0.1.0')
        with self.assertRaises(subprocess.CalledProcessError):
            self.prepare()
        self.assertTrue(self.prepare(dry_run=True)['dry_run'])

    def test_stale_lockfile_and_missing_notes_stop_publication(self):
        lock = self.root / 'Cargo.lock'
        lock.write_text(lock.read_text().replace('0.1.0', '0.0.1'))
        with self.assertRaisesRegex(ValueError, 'Cargo.lock'):
            self.prepare()
        lock.write_text(lock.read_text().replace('0.0.1', '0.1.0'))
        (self.root / 'CHANGELOG.md').write_text('# Changelog\n\n## Unreleased\n')
        with self.assertRaisesRegex(ValueError, 'CHANGELOG.md'):
            self.prepare()


class IntegrityTests(unittest.TestCase):
    def test_all_five_archives_round_trip_and_detect_corruption(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            metadata = assets(directory)
            release.validate_metadata(metadata, '0.1.0', COMMIT, IMAGES)
            release.verify_assets(directory, metadata)
            archive = directory / metadata['artifacts'][0]['archive']
            archive.write_bytes(archive.read_bytes() + b'tampered')
            with self.assertRaisesRegex(ValueError, 'Archive checksum'):
                release.verify_assets(directory, metadata)

    def test_binary_checksum_is_checked_even_with_valid_archive_checksum(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            metadata = assets(directory)
            metadata['artifacts'][0]['binary_sha256'] = 'f' * 64
            with self.assertRaisesRegex(ValueError, 'Binary checksum'):
                release.verify_assets(directory, metadata)

    def test_reject_cross_release_checkpoints_and_missing_platforms(self):
        with tempfile.TemporaryDirectory() as temporary:
            metadata = assets(Path(temporary))
            for field, value in (('version', '0.2.0'), ('tag', 'v0.2.0'),
                                 ('commit', 'c' * 40), ('rust', 'stable')):
                altered = {**metadata, field: value}
                with self.subTest(field=field), self.assertRaises(ValueError):
                    release.validate_metadata(altered, '0.1.0', COMMIT, IMAGES)
            with self.assertRaises(ValueError):
                release.validate_metadata({**metadata, 'artifacts': metadata['artifacts'][:-1]},
                                          '0.1.0', COMMIT, IMAGES)
            altered = copy.deepcopy(metadata)
            altered['artifacts'][0]['archive'] = '../../archive.tar.gz'
            with self.assertRaises(ValueError):
                release.validate_metadata(altered, '0.1.0', COMMIT, IMAGES)
            with self.assertRaises(ValueError):
                release.validate_metadata(metadata, '0.1.0', COMMIT, {**IMAGES, 'dockerhub': 'docker.io/other/ponydb'})

    def test_reject_archive_traversal_symlinks_and_duplicate_entries(self):
        target = release.TARGETS[0]
        prefix = f'ponydb-0.1.0-{target}/'
        for name, kind in ((prefix + '../escape', tarfile.REGTYPE),
                           (prefix + 'link', tarfile.SYMTYPE),
                           ('wrong-root/ponydb', tarfile.REGTYPE),
                           (prefix + 'C:/escape', tarfile.REGTYPE),
                           (prefix + 'duplicate', 'duplicate')):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                archive = directory / 'bad.tar.gz'
                with tarfile.open(archive, 'w:gz') as stream:
                    entry = tarfile.TarInfo(name)
                    entry.type = tarfile.REGTYPE if kind == 'duplicate' else kind
                    stream.addfile(entry, io.BytesIO())
                    if kind == 'duplicate':
                        stream.addfile(entry, io.BytesIO())
                with self.assertRaises(ValueError):
                    release.unpack(archive, directory / 'unpack', '0.1.0', target)
                self.assertFalse((directory / 'escape').exists())

    def test_registry_digest_includes_the_index_and_attestations(self):
        manifest = json.dumps({'manifests': [
            {'platform': {'os': 'linux', 'architecture': 'amd64'}},
            {'platform': {'os': 'linux', 'architecture': 'arm64'}},
            {'platform': {'os': 'unknown', 'architecture': 'unknown'}},
        ]}).encode()
        with patch.object(release.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, manifest, b'')):
            self.assertEqual(release.inspect('example/image:tag'), 'sha256:' + hashlib.sha256(manifest).hexdigest())
        with patch.object(release.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, b'{}', b'')):
            with self.assertRaisesRegex(ValueError, 'architectures'):
                release.inspect('example/image:tag')

    def test_registry_auth_errors_are_not_treated_as_missing_images(self):
        for message, missing in ((b'manifest unknown', True), (b'unauthorized: access denied', False),
                                 (b'connection timed out', False)):
            result = subprocess.CompletedProcess([], 1, b'', message)
            with self.subTest(message=message), patch.object(release.subprocess, 'run', return_value=result):
                if missing:
                    self.assertIsNone(release.inspect('example/image:tag', missing_ok=True))
                else:
                    with self.assertRaises(RuntimeError):
                        release.inspect('example/image:tag', missing_ok=True)


class RecoveryTests(unittest.TestCase):
    def test_first_draft_creation_uses_list_api_and_uploads_the_checkpoint(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            metadata = assets(directory)
            metadata['images']['digest'] = None
            release.write_json(directory / 'release-metadata.json', metadata)
            states = []

            def api(request, **kwargs):
                self.assertTrue(request.full_url.endswith('/releases?per_page=100&page=1'))
                return io.BytesIO(json.dumps(states).encode())

            def command(*args, **kwargs):
                self.assertEqual(args[:2], ('gh', 'release'))
                if args[2] == 'create':
                    states.append({'tag_name': 'v0.1.0', 'draft': True, 'assets': []})
                else:
                    self.assertEqual(args[2], 'upload')
                    states[0]['assets'].append({'name': Path(args[4]).name})

            with patch.dict(os.environ, {'GH_TOKEN': 'fixture-token'}), \
                    patch.object(release.urllib.request, 'urlopen', side_effect=api), \
                    patch.object(release, 'notes', return_value='First release.'), \
                    patch.object(release, 'run', side_effect=command):
                release.draft(argparse.Namespace(assets=directory))
            self.assertTrue(release.has_checkpoint(states[0], '0.1.0'))

    def test_stage_retry_restores_digest_after_interrupted_registry_copy(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            remote, local = root / 'remote', root / 'local'
            remote.mkdir()
            metadata = assets(remote)
            metadata['images']['digest'] = None
            release.write_json(remote / 'release-metadata.json', metadata)
            state = {'tag_name': 'v0.1.0', 'draft': True,
                     'assets': [{'name': name} for name in release.checkpoint_names('0.1.0')]}
            args = argparse.Namespace(assets=local, binaries=root / 'binaries', restore=True)

            def download(tag, destination, patterns):
                self.assertEqual(tag, 'v0.1.0')
                destination.mkdir(parents=True, exist_ok=True)
                for pattern in patterns:
                    for source in remote.glob(pattern):
                        shutil.copy2(source, destination / source.name)

            def command(*command, **kwargs):
                if command[:3] == ('gh', 'release', 'upload'):
                    shutil.copy2(command[4], remote / Path(command[4]).name)

            with patch.object(release, 'workspace_version', return_value='0.1.0'), \
                    patch.object(release, 'capture', return_value=COMMIT), \
                    patch.dict(os.environ, {'GHCR_IMAGE': IMAGES['ghcr'], 'DOCKERHUB_IMAGE': IMAGES['dockerhub']}), \
                    patch.object(release, 'release', return_value=state), \
                    patch.object(release, 'download', side_effect=download), \
                    patch.object(release, 'run', side_effect=command) as commands, \
                    patch.object(release, 'output') as output:
                release.assemble(args)
                with patch.object(release, 'inspect', side_effect=[DIGEST, None]), \
                        patch.object(release, 'copy_image', side_effect=RuntimeError('registry unavailable')):
                    with self.assertRaisesRegex(RuntimeError, 'registry unavailable'):
                        release.candidate(args)
                self.assertEqual(json.loads((remote / 'release-metadata.json').read_text())['images']['digest'], DIGEST)
                # A rerun starts on a fresh runner with stale prepare outputs.
                shutil.rmtree(local)
                release.recovery(argparse.Namespace())
                self.assertTrue(output.call_args.kwargs['reuse'])
                release.assemble(args)
                release.draft(args)
                commands.reset_mock()
                with patch.object(release, 'inspect', side_effect=[DIGEST, None]), \
                        patch.object(release, 'copy_image') as copy_image:
                    release.candidate(args)
                    copy_image.assert_called_once_with(IMAGES['ghcr'] + '@' + DIGEST,
                                                       IMAGES['dockerhub'] + ':sha-' + COMMIT)
                commands.assert_not_called()

    def test_recovery_only_reuses_complete_draft_checkpoints(self):
        names = release.checkpoint_names('0.1.0')
        complete = {'draft': True, 'assets': [{'name': name} for name in names]}
        partial = {**complete, 'assets': [{'name': name} for name in names - {'SHA256SUMS'}]}
        with patch.object(release, 'workspace_version', return_value='0.1.0'), \
                patch.object(release, 'output') as output:
            for state, expected in ((None, False), (partial, False), (complete, True)):
                with self.subTest(state=state), patch.object(release, 'release', return_value=state):
                    release.recovery(argparse.Namespace())
                    self.assertEqual(output.call_args.kwargs['reuse'], expected)
            with patch.object(release, 'release', return_value={**complete, 'draft': False}):
                with self.assertRaisesRegex(ValueError, 'already published'):
                    release.recovery(argparse.Namespace())

    def test_docker_hub_failure_reuses_checkpoint_without_rebuilding(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            metadata = assets(directory)
            args = argparse.Namespace(assets=directory, binaries=directory / 'binaries')
            with patch.object(release, 'inspect', side_effect=[DIGEST, None]), \
                    patch.object(release, 'copy_image', side_effect=RuntimeError('registry unavailable')), \
                    patch.object(release, 'run') as commands, patch.object(release, 'checkpoint') as checkpoint:
                with self.assertRaisesRegex(RuntimeError, 'registry unavailable'):
                    release.candidate(args)
                commands.assert_not_called()
                checkpoint.assert_not_called()
            self.assertEqual(json.loads((directory / 'release-metadata.json').read_text()), metadata)

    def test_mismatching_version_tag_stops_before_aliases_or_release_publication(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            assets(directory)
            with patch.object(release, 'inspect', side_effect=[DIGEST, DIGEST, 'sha256:' + 'c' * 64]), \
                    patch.object(release, 'copy_image') as copy_image, patch.object(release, 'run') as commands:
                with self.assertRaisesRegex(ValueError, 'Refusing to replace version'):
                    release.promote(argparse.Namespace(assets=directory))
                copy_image.assert_not_called()
                commands.assert_not_called()

    def test_prerelease_checks_only_exact_version_tags(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            assets(directory, '0.2.0-rc.1')
            with patch.object(release, 'inspect', return_value=DIGEST) as inspect_image, \
                    patch.object(release, 'release', return_value={'draft': False}), \
                    patch.object(release, 'copy_image') as copy_image, patch.object(release, 'run') as commands:
                release.promote(argparse.Namespace(assets=directory))
                self.assertTrue(all(':latest' not in call.args[0] for call in inspect_image.call_args_list))
                self.assertTrue(all(not call.args[0].endswith(':0.2') for call in inspect_image.call_args_list))
                copy_image.assert_not_called()
                commands.assert_not_called()

    def test_draft_resume_preserves_existing_archives(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            metadata = assets(directory)
            names = [entry['archive'] for entry in metadata['artifacts']] + ['SHA256SUMS', 'release-metadata.json']
            state = {'draft': True, 'assets': [{'name': name} for name in names]}

            def download(_tag, destination, patterns):
                for name in patterns:
                    shutil.copyfile(directory / name, destination / name)

            with patch.object(release, 'release', return_value=state), \
                    patch.object(release, 'download', side_effect=download), patch.object(release, 'run') as commands:
                release.draft(argparse.Namespace(assets=directory))
                commands.assert_not_called()

    def test_latest_release_ignores_drafts_and_compares_all_pages(self):
        older = {'tag_name': 'v0.9.0', 'draft': False, 'prerelease': False}
        newer = {'tag_name': 'v0.10.0', 'draft': False, 'prerelease': False}
        with patch.object(release, 'github', side_effect=[[older] * 100, [newer]]) as api:
            self.assertFalse(release.latest_release('0.9.10'))
            self.assertEqual(api.call_count, 2)
        with patch.object(release, 'github', return_value=[{**newer, 'draft': True}]):
            self.assertTrue(release.latest_release('0.9.10'))


if __name__ == '__main__':
    unittest.main()
