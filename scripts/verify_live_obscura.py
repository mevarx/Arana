#!/usr/bin/env python3
"""Verify pinned install, real JS rendering, and owned-only removal through the CLI."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import urllib.request
from html.parser import HTMLParser

class QuoteCounter(HTMLParser):
 def __init__(self):
  super().__init__();self.count=0
 def handle_starttag(self,tag,attrs):
  if tag=='div' and 'quote' in (dict(attrs).get('class') or '').split():self.count+=1

def count_quote_nodes(source):
 parser=QuoteCounter();parser.feed(source);return parser.count

ROOT=Path(__file__).resolve().parents[1]

def main():
 p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,default=ROOT/'target/debug'/('arana.exe' if os.name=='nt' else 'arana'));p.add_argument('--out',type=Path,default=ROOT/'artifacts/obscura-live');args=p.parse_args();args.out.mkdir(parents=True,exist_ok=True)
 report={'url':'https://quotes.toscrape.com/js/','steps':[],'passed':False}
 with tempfile.TemporaryDirectory(prefix='state-',dir=args.out) as state:
  root=Path(state);env={**os.environ,'ARANA_HOME':state}
  # No executable/checksum/mirror override is allowed in this real release check.
  for k in ('ARANA_OBSCURA_BIN','ARANA_OBSCURA_SHA256','ARANA_TOOL_REGISTRY'):env.pop(k,None)
  def run(name,cli,timeout):
   o=subprocess.run([str(args.binary.resolve()),*cli],env=env,capture_output=True,timeout=timeout)
   (args.out/f'{name}.stdout.json').write_bytes(o.stdout);(args.out/f'{name}.stderr.log').write_bytes(o.stderr)
   v=json.loads(o.stdout);report['steps'].append({'step':name,'exit_code':o.returncode,'ok':v.get('ok')});return v
  install=run('install',['add','obscura'],660)
  if install.get('ok'):
   ledger=json.loads((root/'state.json').read_text(encoding='utf-8'));entry=ledger['tools']['obscura'];report['installed_version']=entry['version'];files=entry['files'];report['installed_file_count']=len(files);report['all_recorded_files_exist']=all((root/f).is_file() for f in files)
   req=urllib.request.Request(report['url'],headers={'User-Agent':'arana-verification'})
   with urllib.request.urlopen(req,timeout=30) as response:raw=response.read(5*1024*1024).decode('utf-8')
   (args.out/'raw-page.html').write_text(raw,encoding='utf-8');report['raw_quote_divs']=count_quote_nodes(raw)
   read=run('read',['web','read',report['url'],'--backend','obscura','--timeout','60'],75)
   text='\n'.join(i.get('text') or '' for i in read.get('items',[]));report['backend']=read.get('backend');report['rendered_chars']=len(text);report['rendered_open_quotes']=text.count('“');report['raw_lacks_rendered_quote']=('“' not in raw) and ('“' in text)
   marker=root/'tools/obscura/bin/user-kept.txt';marker.write_text('not installed by arana',encoding='utf-8')
   removed=run('remove',['remove','obscura'],30)
   after=json.loads((root/'state.json').read_text(encoding='utf-8'));report['owned_files_gone']=all(not(root/f).exists() for f in files);report['user_file_preserved']=marker.read_text(encoding='utf-8')=='not installed by arana';report['ledger_entry_removed']='obscura' not in after['tools']
   report['passed']=all(step['ok'] for step in report['steps']) and report['all_recorded_files_exist'] and report['backend']=='obscura' and report['raw_quote_divs']==0 and report['rendered_open_quotes']>=1 and report['owned_files_gone'] and report['user_file_preserved'] and report['ledger_entry_removed']
 (args.out/'summary.json').write_text(json.dumps(report,indent=2),encoding='utf-8');print(json.dumps(report));return 0 if report['passed'] else 1
if __name__=='__main__':raise SystemExit(main())
