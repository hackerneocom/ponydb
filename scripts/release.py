#!/usr/bin/env python3
"""Build, package, checkpoint, and publish ponydb releases. Python 3.11+, stdlib only."""
import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import urllib.error
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
RUST = '1.95.0'
REPOSITORY = 'hackerneocom/ponydb'
TARGETS = (
    'x86_64-unknown-linux-musl', 'aarch64-unknown-linux-musl',
    'x86_64-apple-darwin', 'aarch64-apple-darwin', 'x86_64-pc-windows-msvc',
)
VERSION = re.compile(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'
                     r'(?:-(alpha|beta|rc)\.(0|[1-9][0-9]*))?\Z')
DIGEST = re.compile(r'sha256:[0-9a-f]{64}\Z')


def require(condition, message):
    if not condition:
        raise ValueError(message)


def run(*args, **kwargs):
    return subprocess.run([str(arg) for arg in args], check=True, cwd=ROOT,
                          timeout=kwargs.pop('timeout', 600), **kwargs)


def capture(*args, **kwargs):
    return run(*args, stdout=subprocess.PIPE, **kwargs).stdout.decode().strip()


def sha256(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n', encoding='utf-8')


def output(**values):
    for key, value in values.items():
        if isinstance(value, bool):
            value = str(value).lower()
        print(f'{key}={value}', flush=True)
        if os.environ.get('GITHUB_OUTPUT'):
            with open(os.environ['GITHUB_OUTPUT'], 'a', encoding='utf-8') as stream:
                stream.write(f'{key}={value}\n')


def version_key(version):
    match = VERSION.fullmatch(version)
    require(match is not None, f'Unsupported release version: {version}')
    major, minor, patch, channel, serial = match.groups()
    return (int(major), int(minor), int(patch),
            {'alpha': 0, 'beta': 1, 'rc': 2, None: 3}[channel], int(serial or 0))


def workspace_version():
    with (ROOT / 'Cargo.toml').open('rb') as stream:
        version = tomllib.load(stream)['workspace']['package']['version']
    version_key(version)
    return version


def notes(version):
    text = (ROOT / 'CHANGELOG.md').read_text(encoding='utf-8')
    match = re.search(r'^## ' + re.escape(version) + r'\s*\n(.*?)(?=^## |\Z)',
                      text, flags=re.MULTILINE | re.DOTALL)
    require(match is not None and match[1].strip(), f'Add CHANGELOG.md notes for {version}')
    return match[1].strip()


def github(path):
    request = urllib.request.Request('https://api.github.com/repos/' + REPOSITORY + '/' + path,
                                     headers={'Authorization': 'Bearer ' + os.environ['GH_TOKEN'],
                                              'Accept': 'application/vnd.github+json',
                                              'X-GitHub-Api-Version': '2022-11-28'})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise


def release(tag):
    # The tag endpoint only returns published releases. Listing also finds drafts
    # when called with a token that has push access.
    page = 1
    while True:
        states = github(f'releases?per_page=100&page={page}')
        require(states is not None, 'Cannot list repository releases')
        for state in states:
            if state['tag_name'] == tag:
                return state
        if len(states) < 100:
            return None
        page += 1


def checkpoint_names(version):
    return {archive_name(version, target) for target in TARGETS} | {'release-metadata.json', 'SHA256SUMS'}


def has_checkpoint(state, version):
    return bool(state and checkpoint_names(version) <= {asset['name'] for asset in state['assets']})


def recovery(args):
    version = workspace_version()
    state = release('v' + version)
    require(not state or state['draft'], 'Cannot modify an already published release')
    output(reuse=has_checkpoint(state, version))


def download(tag, directory, patterns):
    directory.mkdir(parents=True, exist_ok=True)
    command = ['gh', 'release', 'download', tag, '--repo', REPOSITORY,
               '--dir', directory, '--clobber']
    for pattern in patterns:
        command += ['--pattern', pattern]
    run(*command)


def prepare(args):
    version = workspace_version()
    tag = args.tag or os.environ.get('GITHUB_REF_NAME', '')
    commit = capture('git', 'rev-parse', 'HEAD')
    if not args.dry_run:
        require(os.environ.get('GITHUB_REPOSITORY') == REPOSITORY, 'Publishing is restricted to ' + REPOSITORY)
        require(tag == 'v' + version, 'Tag must be v' + version)
        require(capture('git', 'rev-parse', 'refs/tags/' + tag + '^{commit}') == commit,
                'Checkout does not match the release tag')
        require(os.environ.get('GITHUB_REF') == 'refs/tags/' + tag,
                'Select the same tag as the workflow ref so build attestations identify the correct source')
        run('git', 'merge-base', '--is-ancestor', commit, 'origin/main')
    else:
        tag = 'v' + version
    notes(version)
    packages = json.loads(capture('cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1'))['packages']
    with (ROOT / 'Cargo.lock').open('rb') as stream:
        locked = {(package['name'], package['version']) for package in tomllib.load(stream)['package']
                  if 'source' not in package}
    require(all((package['name'], package['version']) in locked for package in packages),
            'Update all workspace package versions in Cargo.lock before tagging')
    namespace = os.environ.get('DOCKERHUB_NAMESPACE', '').strip()
    require(args.dry_run or re.fullmatch(r'[a-z0-9][a-z0-9_-]*', namespace),
            'Configure DOCKERHUB_NAMESPACE before publishing')
    images = {'ghcr': 'ghcr.io/' + REPOSITORY,
              'dockerhub': 'docker.io/' + (namespace or 'dry-run') + '/ponydb'}
    output(version=version, tag=tag, commit=commit, prerelease='-' in version,
           dry_run=args.dry_run, asset_names=json.dumps(sorted(checkpoint_names(version))),
           ghcr=images['ghcr'], dockerhub=images['dockerhub'])


def build(args):
    target = args.target
    env = os.environ.copy()
    prefix = 'CARGO_TARGET_' + target.upper().replace('-', '_')
    if target.endswith('-musl'):
        env['CC_' + target.replace('-', '_')] = 'musl-gcc'
        # rustc supplies musl's startup objects and libc. musl-gcc's specs can
        # inject a dynamic interpreter for -static-pie, even with crt-static.
        env[prefix + '_LINKER'] = 'cc'
    if target.endswith(('-musl', '-msvc')):
        env[prefix + '_RUSTFLAGS'] = '-C target-feature=+crt-static'
    require(capture('rustc', '--version').startswith('rustc ' + RUST + ' '), 'Use Rust ' + RUST)
    run('cargo', 'build', '--locked', '--release', '-p', 'ponydb', '--target', target,
        env=env, timeout=3600)


def archive_name(version, target):
    return f'ponydb-{version}-{target}' + ('.zip' if target.endswith('-msvc') else '.tar.gz')


def executable(target):
    return 'ponydb.exe' if target.endswith('-msvc') else 'ponydb'


def unpack(archive, destination, version, target):
    """Extract only regular files beneath the expected package root."""
    prefix = f'ponydb-{version}-{target}/'
    if archive.suffix == '.zip':
        with zipfile.ZipFile(archive) as stream:
            entries = [(entry.filename, stream.read(entry)) for entry in stream.infolist()]
    else:
        with tarfile.open(archive) as stream:
            entries = []
            for entry in stream.getmembers():
                require(entry.isfile(), 'Archive must contain regular files only')
                entries.append((entry.name, stream.extractfile(entry).read()))
    seen = set()
    for name, data in entries:
        require(name.startswith(prefix), 'Archive root does not match version/target')
        relative = name[len(prefix):]
        require(relative and not any(part in ('', '.', '..') for part in relative.split('/'))
                and '\\' not in relative and ':' not in relative, 'Unsafe archive path')
        require(relative not in seen, 'Duplicate archive entry')
        seen.add(relative)
        path = destination / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    require({executable(target), 'LICENSE', 'README.md', 'examples/ponydb.toml'} <= seen,
            'Package is missing required files')
    binary = destination / executable(target)
    binary.chmod(0o755)
    return binary


def pack(args):
    version = workspace_version()
    target, directory = args.target, args.assets.resolve()
    directory.mkdir(parents=True, exist_ok=True)
    binary = ROOT / 'target' / target / 'release' / executable(target)
    if target.endswith('-musl'):
        require('INTERP' not in capture('readelf', '-l', binary), 'musl binary has a dynamic interpreter')
        require('(NEEDED)' not in capture('readelf', '-d', binary), 'musl binary depends on shared libraries')
    diagnostic = json.loads(capture(binary, 'version'))
    require(diagnostic['program'] == version and diagnostic['sqlite_version'], 'Invalid binary version/SQLite')
    files = {executable(target): binary, 'LICENSE': ROOT / 'LICENSE',
             'README.md': ROOT / 'README.md', 'README.zh-CN.md': ROOT / 'README.zh-CN.md',
             'examples/ponydb.toml': ROOT / 'examples/ponydb.toml'}
    archive = directory / archive_name(version, target)
    prefix = f'ponydb-{version}-{target}/'
    if target.endswith('-msvc'):
        with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED) as stream:
            for name, path in files.items():
                info = zipfile.ZipInfo(prefix + name, date_time=(1980, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED
                stream.writestr(info, path.read_bytes())
    else:
        with archive.open('wb') as raw, gzip.GzipFile(fileobj=raw, mode='wb', filename='', mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode='w') as stream:
                for name, path in files.items():
                    info = tarfile.TarInfo(prefix + name)
                    info.size = path.stat().st_size
                    info.mode = 0o755 if name == executable(target) else 0o644
                    with path.open('rb') as data:
                        stream.addfile(info, data)
    record = {'target': target, 'archive': archive.name, 'sha256': sha256(archive),
              'binary_sha256': sha256(binary)}
    write_json(directory / ('build-' + target + '.json'),
               {'version': version, 'commit': capture('git', 'rev-parse', 'HEAD'), 'rust': RUST, **record})
    extracted = unpack(archive, directory / 'unpacked' / target, version, target)
    require(sha256(extracted) == record['binary_sha256'], 'Packaged binary differs from build')
    output(binary=extracted, archive=archive)


def validate_metadata(metadata, version, commit, images):
    require(metadata['version'] == version and metadata['tag'] == 'v' + version
            and metadata['commit'] == commit and metadata['rust'] == RUST,
            'Release checkpoint does not match version, source, or toolchain')
    require({entry['target'] for entry in metadata['artifacts']} == set(TARGETS)
            and len(metadata['artifacts']) == len(TARGETS), 'Release needs exactly five CLI packages')
    for entry in metadata['artifacts']:
        require(entry['archive'] == archive_name(version, entry['target']), 'Invalid archive name')
        for field in ('sha256', 'binary_sha256'):
            require(re.fullmatch(r'[0-9a-f]{64}', entry[field]), 'Invalid artifact checksum')
    require(all(metadata['images'][key] == value for key, value in images.items()),
            'Registry configuration differs from release checkpoint')
    digest = metadata['images'].get('digest')
    require(digest is None or DIGEST.fullmatch(digest), 'Invalid image digest')


def verify_assets(directory, metadata):
    expected = ''
    for entry in metadata['artifacts']:
        require(sha256(directory / entry['archive']) == entry['sha256'], 'Archive checksum mismatch')
        with tempfile.TemporaryDirectory() as temporary:
            binary = unpack(directory / entry['archive'], Path(temporary), metadata['version'], entry['target'])
            require(sha256(binary) == entry['binary_sha256'], 'Binary checksum mismatch')
        expected += entry['sha256'] + '  ' + entry['archive'] + '\n'
    require((directory / 'SHA256SUMS').read_text(encoding='utf-8') == expected, 'SHA256SUMS differs from metadata')


def assemble(args):
    directory = args.assets.resolve()
    version, commit = workspace_version(), capture('git', 'rev-parse', 'HEAD')
    images = {'ghcr': os.environ['GHCR_IMAGE'], 'dockerhub': os.environ['DOCKERHUB_IMAGE']}
    if args.restore:
        download('v' + version, directory, ['ponydb-*.tar.gz', 'ponydb-*.zip', 'SHA256SUMS', 'release-metadata.json'])
        metadata = json.loads((directory / 'release-metadata.json').read_text(encoding='utf-8'))
    else:
        entries = []
        for target in TARGETS:
            record = json.loads((directory / ('build-' + target + '.json')).read_text(encoding='utf-8'))
            require(record.pop('version') == version and record.pop('commit') == commit
                    and record.pop('rust') == RUST and record['target'] == target,
                    'Build artifact belongs to a different release')
            entries.append(record)
        metadata = {'version': version, 'tag': 'v' + version, 'commit': commit, 'rust': RUST,
                    'artifacts': entries, 'images': {**images, 'digest': None}}
        write_json(directory / 'release-metadata.json', metadata)
        (directory / 'SHA256SUMS').write_text(''.join(entry['sha256'] + '  ' + entry['archive'] + '\n'
                                                   for entry in entries), encoding='utf-8')
    validate_metadata(metadata, version, commit, images)
    verify_assets(directory, metadata)
    context(directory, metadata, args.binaries.resolve())


def context(directory, metadata, destination):
    for entry in metadata['artifacts']:
        if entry['target'].endswith('-musl'):
            arch = 'amd64' if entry['target'].startswith('x86_64') else 'arm64'
            with tempfile.TemporaryDirectory() as temporary:
                binary = unpack(directory / entry['archive'], Path(temporary), metadata['version'], entry['target'])
                (destination / arch).mkdir(parents=True, exist_ok=True)
                shutil.copy2(binary, destination / arch / 'ponydb')


def checkpoint(directory, metadata):
    state = release(metadata['tag'])
    require(state and state['draft'], 'Only draft release checkpoints may be updated')
    write_json(directory / 'release-metadata.json', metadata)
    run('gh', 'release', 'upload', metadata['tag'], directory / 'release-metadata.json',
        '--repo', REPOSITORY, '--clobber')


def draft(args):
    directory = args.assets.resolve()
    metadata = json.loads((directory / 'release-metadata.json').read_text(encoding='utf-8'))
    verify_assets(directory, metadata)
    state = release(metadata['tag'])
    if not state:
        text = notes(metadata['version']) + '\n\n'
        text += ('Downloads: choose your target below and verify it against `SHA256SUMS`.\n\n'
                 'Source: https://github.com/' + REPOSITORY + '/tree/' + metadata['commit'] + '\n\n')
        for image in (metadata['images']['ghcr'], metadata['images']['dockerhub']):
            text += '`docker pull ' + image + ':' + metadata['version'] + '`\n\n'
        text += '[Installation and compatibility](https://github.com/' + REPOSITORY + '/blob/'
        text += metadata['commit'] + '/skills/ponydb/references/installation.md)\n'
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'notes.md'
            path.write_text(text, encoding='utf-8')
            run('gh', 'release', 'create', metadata['tag'], '--repo', REPOSITORY, '--verify-tag',
                '--draft', '--title', metadata['tag'], '--notes-file', path)
        state = release(metadata['tag'])
    require(state and state['draft'], 'Release draft is missing or already published')
    existing = {asset['name'] for asset in state['assets']}
    for name in [entry['archive'] for entry in metadata['artifacts']] + ['SHA256SUMS']:
        if name in existing:
            with tempfile.TemporaryDirectory() as temporary:
                download(metadata['tag'], Path(temporary), [name])
                require(sha256(Path(temporary) / name) == sha256(directory / name),
                        'Refusing to replace existing release asset ' + name)
        else:
            run('gh', 'release', 'upload', metadata['tag'], directory / name, '--repo', REPOSITORY)
    if 'release-metadata.json' in existing:
        with tempfile.TemporaryDirectory() as temporary:
            download(metadata['tag'], Path(temporary), ['release-metadata.json'])
            previous = json.loads((Path(temporary) / 'release-metadata.json').read_text())
            require(previous == metadata, 'Restore the existing checkpoint before continuing')
    else:
        checkpoint(directory, metadata)


def inspect(reference, missing_ok=False, anonymous=False):
    command = ['skopeo', 'inspect', '--raw']
    with tempfile.TemporaryDirectory() as temporary:
        if anonymous:
            auth = Path(temporary) / 'auth.json'
            write_json(auth, {'auths': {}})
            command += ['--authfile', str(auth)]
        result = subprocess.run(command + ['docker://' + reference], cwd=ROOT,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120)
    if result.returncode:
        error = result.stderr.decode()
        if missing_ok and any(message in error.lower() for message in ('manifest unknown', 'name unknown')):
            return None
        raise RuntimeError('Cannot inspect ' + reference + ': ' + error)
    manifest = json.loads(result.stdout)
    platforms = {(entry.get('platform', {}).get('os'), entry.get('platform', {}).get('architecture'))
                 for entry in manifest.get('manifests', [])}
    require(platforms - {('unknown', 'unknown')} == {('linux', 'amd64'), ('linux', 'arm64')},
            'Image index must contain exactly both Linux architectures (plus attestations)')
    return 'sha256:' + hashlib.sha256(result.stdout).hexdigest()


def copy_image(source, destination):
    run('skopeo', 'copy', '--all', '--preserve-digests', '--retry-times', '3',
        'docker://' + source, 'docker://' + destination, timeout=1800)
    require(inspect(source) == inspect(destination), 'Registry copy changed the image digest')


def candidate(args):
    directory = args.assets.resolve()
    metadata = json.loads((directory / 'release-metadata.json').read_text(encoding='utf-8'))
    images, version = metadata['images'], metadata['version']
    ref = images['ghcr'] + ':sha-' + metadata['commit']
    digest = images.get('digest')
    if digest:
        require(inspect(images['ghcr'] + '@' + digest) == digest, 'Checkpoint image is unavailable')
    else:
        # A new GHCR package may reject even authenticated reads until its first
        # push. Before a digest is checkpointed only this temporary tag exists;
        # a crash in this interval may rebuild it, never an advertised version.
        run('docker', 'buildx', 'build', '--platform', 'linux/amd64,linux/arm64',
            '--target', 'release', '--build-context', 'binaries=' + str(args.binaries.resolve()),
            '--build-arg', 'VERSION=' + version, '--build-arg', 'REVISION=' + metadata['commit'],
            '--provenance', 'mode=max', '--tag', ref, '--push', '.', timeout=3600)
        digest = inspect(ref)
        images['digest'] = digest
        checkpoint(directory, metadata)
    source = images['ghcr'] + '@' + digest
    target = images['dockerhub'] + ':sha-' + metadata['commit']
    existing = inspect(target, missing_ok=True)
    require(existing is None or existing == digest, 'Docker Hub candidate already has a different digest')
    if existing is None:
        copy_image(source, target)
    output(digest=digest)


def alias_allowed(current, version):
    require('-' not in version, 'Prerelease cannot advance a stable alias')
    return version_key(current) <= version_key(version)


def promote(args):
    directory = args.assets.resolve()
    metadata = json.loads((directory / 'release-metadata.json').read_text(encoding='utf-8'))
    images, version = metadata['images'], metadata['version']
    digest = images['digest']
    require(digest and DIGEST.fullmatch(digest), 'Candidate digest is missing')
    # Require anonymous pulls before users are directed to either registry.
    for key in ('ghcr', 'dockerhub'):
        require(inspect(images[key] + '@' + digest, anonymous=True) == digest,
                'Candidate is not publicly readable')
    for key in ('ghcr', 'dockerhub'):
        image = images[key]
        full = image + ':' + version
        existing = inspect(full, missing_ok=True)
        require(existing is None or existing == digest, 'Refusing to replace version tag ' + full)
        if existing is None:
            copy_image(image + '@' + digest, full)
    # Only change aliases once both immutable version tags are available.
    if '-' not in version:
        minor = '.'.join(version.split('.')[:2])
        for key in ('ghcr', 'dockerhub'):
            image = images[key]
            for alias in (minor, 'latest'):
                ref = image + ':' + alias
                if inspect(ref, missing_ok=True):
                    config = json.loads(capture('skopeo', 'inspect', '--config',
                                               '--override-arch', 'amd64', 'docker://' + ref))
                    current = config['config']['Labels']['org.opencontainers.image.version']
                    if not alias_allowed(current, version):
                        print(f'Keep {ref} at newer version {current}', flush=True)
                        continue
                copy_image(image + '@' + digest, ref)
    state = release(metadata['tag'])
    require(state is not None, 'Release draft is missing')
    if state['draft']:
        # Download and validate the checkpoint immediately before freezing the release.
        with tempfile.TemporaryDirectory() as temporary:
            download(metadata['tag'], Path(temporary),
                     ['ponydb-*.tar.gz', 'ponydb-*.zip', 'SHA256SUMS', 'release-metadata.json'])
            require(json.loads((Path(temporary) / 'release-metadata.json').read_text()) == metadata,
                    'Remote release checkpoint changed')
            verify_assets(Path(temporary), metadata)
        run('gh', 'release', 'edit', metadata['tag'], '--repo', REPOSITORY, '--draft=false',
            '--prerelease=' + str('-' in version).lower(),
            '--latest=' + str('-' not in version and latest_release(version)).lower())


def latest_release(version):
    page = 1
    while True:
        releases = github(f'releases?per_page=100&page={page}')
        for state in releases:
            other = state['tag_name'].removeprefix('v')
            if not state['draft'] and not state['prerelease'] and VERSION.fullmatch(other):
                if version_key(other) > version_key(version):
                    return False
        if len(releases) < 100:
            return True
        page += 1


def published(args):
    with tempfile.TemporaryDirectory() as temporary:
        directory = Path(temporary)
        tag = 'v' + workspace_version()
        download(tag, directory, ['ponydb-*.tar.gz', 'ponydb-*.zip', 'SHA256SUMS', 'release-metadata.json'])
        metadata = json.loads((directory / 'release-metadata.json').read_text())
        validate_metadata(metadata, workspace_version(), capture('git', 'rev-parse', 'HEAD'),
                          {'ghcr': os.environ['GHCR_IMAGE'], 'dockerhub': os.environ['DOCKERHUB_IMAGE']})
        verify_assets(directory, metadata)
        for key in ('ghcr', 'dockerhub'):
            require(inspect(metadata['images'][key] + ':' + metadata['version'], anonymous=True)
                    == metadata['images']['digest'], 'Published version tag differs from checkpoint')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    command = commands.add_parser('prepare')
    command.add_argument('--tag', default='')
    command.add_argument('--dry-run', action='store_true')
    for name in ('build', 'pack'):
        command = commands.add_parser(name)
        command.add_argument('--target', choices=TARGETS, required=True)
        if name == 'pack':
            command.add_argument('--assets', type=Path, default=Path('release-assets'))
    for name in ('assemble', 'draft', 'candidate', 'promote'):
        command = commands.add_parser(name)
        command.add_argument('--assets', type=Path, default=Path('release-assets'))
        if name in ('assemble', 'candidate'):
            command.add_argument('--binaries', type=Path, default=Path('release-binaries'))
        if name == 'assemble':
            command.add_argument('--restore', action='store_true')
    commands.add_parser('published')
    commands.add_parser('recovery')
    args = parser.parse_args()
    globals()[args.command](args)


if __name__ == '__main__':
    main()
