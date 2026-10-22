"""Small stand-in for the GitLab and Azure DevOps REST APIs (only the calls gitta-mr-check.sh makes).
State: merge requests from mrs.json; comments kept in memory and dumped to comments.json on every change."""
import json, re, sys, os
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse, parse_qs
STATE = sys.argv[2]
mrs = json.load(open(os.path.join(STATE, 'mrs.json')))
notes = {}      # (provider, mr) -> list of {id, body}
seq = [100]
def dump(): json.dump({f'{k[0]}:{k[1]}': v for k, v in notes.items()}, open(os.path.join(STATE, 'comments.json'), 'w'), indent=1)
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def send(self, obj, code=200):
        b = json.dumps(obj).encode(); self.send_response(code); self.send_header('Content-Type', 'application/json'); self.send_header('Content-Length', str(len(b))); self.end_headers(); self.wfile.write(b)
    def body(self):
        n = int(self.headers.get('Content-Length') or 0); raw = self.rfile.read(n).decode()
        if self.headers.get('Content-Type', '').startswith('application/json'): return json.loads(raw or '{}')
        return {k: v[0] for k, v in parse_qs(raw).items()}
    def route(self, method):
        u = urlparse(self.path); p = u.path
        if p.startswith('/api/v4/'):
            assert self.headers.get('PRIVATE-TOKEN') == 'secret', 'no token'
            if m := re.fullmatch(r'/api/v4/projects/7/merge_requests', p):
                return self.send([{'iid': x['id'], 'target_branch': x['target'], 'title': x['title']} for x in mrs])
            if m := re.fullmatch(r'/api/v4/projects/7/merge_requests/(\d+)/notes(?:/(\d+))?', p):
                key = ('gitlab', int(m.group(1))); lst = notes.setdefault(key, [])
                if method == 'GET': return self.send(lst)
                if method == 'POST': seq[0] += 1; lst.append({'id': seq[0], 'body': self.body()['body']}); dump(); return self.send(lst[-1], 201)
                nid = int(m.group(2))
                if method == 'PUT':
                    for x in lst:
                        if x['id'] == nid: x['body'] = self.body()['body']
                    dump(); return self.send({})
                if method == 'DELETE': notes[key] = [x for x in lst if x['id'] != nid]; dump(); return self.send({}, 204)
        if p.startswith('/org/proj/_apis/git/repositories/repo1/'):
            assert self.headers.get('Authorization') == 'Bearer secret', 'no token'
            if p.endswith('/pullrequests'):
                return self.send({'value': [{'pullRequestId': x['id'], 'targetRefName': 'refs/heads/' + x['target'], 'sourceRefName': 'refs/heads/' + x['source'], 'title': x['title']} for x in mrs]})
            if m := re.fullmatch(r'.*/pullRequests/(\d+)/threads', p):
                key = ('azure', int(m.group(1))); lst = notes.setdefault(key, [])
                if method == 'GET': return self.send({'value': [{'id': x['id'], 'comments': [{'id': 1, 'content': x['body']}]} for x in lst]})
                if method == 'POST': seq[0] += 1; lst.append({'id': seq[0], 'body': self.body()['comments'][0]['content']}); dump(); return self.send({}, 200)
            if m := re.fullmatch(r'.*/pullRequests/(\d+)/threads/(\d+)/comments/1', p):
                key = ('azure', int(m.group(1))); tid = int(m.group(2))
                if method == 'PATCH':
                    for x in notes.get(key, []):
                        if x['id'] == tid: x['body'] = self.body()['content']
                    dump(); return self.send({})
                if method == 'DELETE': notes[key] = [x for x in notes.get(key, []) if x['id'] != tid]; dump(); return self.send({}, 204)
        self.send({'error': 'not found', 'path': p}, 404)
    def do_GET(self): self.route('GET')
    def do_POST(self): self.route('POST')
    def do_PUT(self): self.route('PUT')
    def do_PATCH(self): self.route('PATCH')
    def do_DELETE(self): self.route('DELETE')
HTTPServer(('127.0.0.1', int(sys.argv[1])), H).serve_forever()
