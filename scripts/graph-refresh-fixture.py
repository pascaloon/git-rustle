"""Owned localhost API fixture for graph-refresh benchmarks; never uses real credentials.
Launch hidden with the bundled Python runtime. Writes manifest after configuring an
isolated disposable repository; POST /control/advance, GET /control/stats, and
POST /control/stop are exclusively fixture controls. No Git server is started.
"""
import argparse
import json
import os
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlparse, parse_qs

parser = argparse.ArgumentParser()
parser.add_argument('--directory', required=True)
parser.add_argument('--branches', type=int, default=12)
parser.add_argument('--delay-ms', type=int, default=60)
parser.add_argument('--server-only', type=int, default=1000)
parser.add_argument('--unchanged', action='store_true')
args = parser.parse_args()
owned = Path(args.directory).resolve()
owned.mkdir(parents=True, exist_ok=False)
repo = owned / 'repository'
repo.mkdir()
env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM='1', GIT_TERMINAL_PROMPT='0', GIT_OPTIONAL_LOCKS='0', GIT_AUTHOR_DATE='2000-01-01T00:00:00Z', GIT_COMMITTER_DATE='2000-01-01T00:00:00Z')
def git(*arguments):
    return subprocess.check_output(['git', '-C', str(repo), *arguments], env=env, stderr=subprocess.PIPE,
        creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0).decode().strip()
git('init', '-q', '-b', 'z-ghost')
git('config', 'user.name', 'Graph Refresh Fixture')
git('config', 'user.email', 'graph-refresh@example.invalid')
git('config', 'credential.helper', '')
(repo/'tracked.txt').write_text('base tracked content\n')
git('add', 'tracked.txt')
git('commit', '--allow-empty', '-qm', 'Local fixture base')
base = git('rev-parse', 'HEAD')
for index in range(args.branches-1):
    git('branch', f'branch-{index:03}')
(repo/'tracked.txt').write_text('retained unstaged content\n')
(repo/'staged.txt').write_text('retained staged content\n')
git('add', 'staged.txt')
(repo/'untracked.txt').write_text('retained untracked content\n')
old, new = 'a'*40, ('a' if args.unchanged else 'c')*40
local_names = ['z-ghost'] + [f'branch-{i:03}' for i in range(args.branches-1)]
lock = threading.Lock()
state = {'advanced': False, 'active': 0, 'peakRequests': 0, 'requests': [], 'forbidden': [], 'pipelineRequests': 0, 'projectRequests': 0, 'pullRequests': 0}
def commit(sha, parent):
    return {'sha': sha, 'parents': [{'sha': parent}], 'commit': {
        'author': {'name': 'Fixture', 'email': 'fixture@example.invalid', 'date': '2026-01-01T00:00:00Z'},
        'committer': {'name': 'Fixture', 'email': 'fixture@example.invalid', 'date': '2026-01-01T00:00:00Z'},
        'message': 'Changed ghost' if sha == new else 'Initial ghost'}}
class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass
    def response(self, status, value):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Connection', 'close')
        self.end_headers()
        self.wfile.write(body)
    def do_POST(self):
        if self.path == '/control/advance':
            with lock:
                state['advanced'] = True
            self.response(200, {'advanced': True})
        elif self.path == '/control/stop':
            self.response(200, {'stopping': True})
            threading.Thread(target=server.shutdown, daemon=True).start()
        else:
            self.response(404, {})
    def do_GET(self):
        parsed = urlparse(self.path)
        if parsed.path == '/control/stats':
            with lock:
                snapshot = dict(state)
                snapshot['requests'] = list(state['requests'])
            snapshot['refsUnchanged'] = git('show-ref') == original_refs
            snapshot['filesUnchanged'] = fingerprint() == original_state
            snapshot['fetchHeadAbsent'] = not (repo / '.git' / 'FETCH_HEAD').exists()
            self.response(200, snapshot)
            return
        with lock:
            state['active'] += 1
            state['peakRequests'] = max(state['peakRequests'], state['active'])
            state['requests'].append({'path': self.path, 'at': time.monotonic(), 'advanced': state['advanced']})
            advanced = state['advanced']
        try:
            root = '/repos/team/repo'
            if parsed.path.startswith(root+'/branches/'):
                name = unquote(parsed.path[len(root+'/branches/'):])
                time.sleep(args.delay_ms/1000)
                self.response(200, {'name': name, 'commit': {'sha': (new if advanced else old) if name == 'z-ghost' else base}})
            elif parsed.path == root+'/branches':
                page = int(parse_qs(parsed.query).get('page', ['1'])[0])
                names = local_names + [f'server-only-{i:05}' for i in range(args.server_only)]
                batch = names[(page-1)*100:page*100]
                self.response(200, [{'name':name,'commit':{'sha':(new if advanced else old) if name=='z-ghost' else base}} for name in batch])
            elif parsed.path == root+'/commits':
                sha = parse_qs(parsed.query).get('sha', [''])[0]
                self.response(200, [commit(new, old), commit(old, base)] if sha == new and new != old else [commit(old, base)])
            elif parsed.path == root+'/tags':
                self.response(200, [])
            elif parsed.path == root+'/actions/runs':
                with lock:
                    state['pipelineRequests'] += 1
                    state['forbidden'].append(self.path)
                time.sleep(args.delay_ms/1000)
                self.response(200, {'total_count': 0, 'workflow_runs': []})
            elif parsed.path in [root+'/pulls', root+'/issues', root+'/contributors', root+'/collaborators']:
                if parsed.path == root+'/pulls':
                    with lock:
                        state['pullRequests'] += 1
                    time.sleep(args.delay_ms/1000)
                self.response(200, [])
            elif parsed.path == root:
                with lock:
                    state['projectRequests'] += 1
                time.sleep(args.delay_ms/1000)
                self.response(200, {'id': 1, 'name': 'repo', 'full_name': 'team/repo', 'html_url': 'https://graph-refresh-fixture.invalid/team/repo', 'owner': {'login': 'team'}})
            else:
                if '/actions/' in parsed.path or 'pipeline' in parsed.path or '/check-' in parsed.path:
                    with lock:
                        state['forbidden'].append(self.path)
                self.response(404, {'message': 'Fixture endpoint not implemented'})
        finally:
            with lock:
                state['active'] -= 1
server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
server.daemon_threads = True
endpoint = f'http://127.0.0.1:{server.server_address[1]}'
git('config', 'remote.origin.url', 'https://graph-refresh-fixture.invalid/team/repo')
git('config', 'remote.origin.rustle-provider', 'github')
git('config', 'remote.origin.rustle-api-url', endpoint)
def fingerprint():
    return {'index':(repo/'.git/index').read_bytes().hex() if (repo/'.git/index').exists() else None,
            'objects':git('count-objects','-v'), 'status':git('status','--porcelain=v1'),
            'stashes':git('stash','list','--format=%H'),
            'files':{str(p.relative_to(repo)):p.read_bytes().hex() for p in repo.rglob('*') if p.is_file() and '.git' not in p.relative_to(repo).parts}}
original_refs = git('show-ref')
original_state = fingerprint()
manifest = {'repository': str(repo), 'controlUrl': endpoint+'/control', 'baseOid': base, 'oldGhostOid': old, 'newGhostOid': new, 'branches': args.branches, 'delayMs': args.delay_ms, 'pid': os.getpid()}
(owned/'manifest.json').write_text(json.dumps(manifest, indent=2))
try:
    server.serve_forever(poll_interval=0.05)
finally:
    with lock:
        final = dict(state)
    (owned/'server-final.json').write_text(json.dumps(final, indent=2))
    server.server_close()
