#!/usr/bin/env python3
"""Measure fresh-process catalog startup and the actual release artifact size."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import tempfile
import time
ROOT=Path(__file__).resolve().parents[1]
def main():
 p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,default=ROOT/'target/release'/('arana.exe' if os.name=='nt' else 'arana'));p.add_argument('--samples',type=int,default=40);p.add_argument('--out',type=Path,default=ROOT/'artifacts/benchmark.json');a=p.parse_args();a.out.parent.mkdir(parents=True,exist_ok=True)
 if a.samples<20:p.error('at least 20 samples are required')
 data=a.binary.read_bytes();samples=[]
 with tempfile.TemporaryDirectory(dir=a.out.parent,prefix='bench-state-') as state:
  for i in range(a.samples+3):
   started=time.perf_counter_ns();r=subprocess.run([str(a.binary.resolve()),'channels'],env={**os.environ,'ARANA_HOME':state},capture_output=True,timeout=5);ms=(time.perf_counter_ns()-started)/1_000_000
   if r.returncode:raise RuntimeError(r.stderr.decode('utf-8',errors='replace'))
   v=json.loads(r.stdout);assert len(v['channels'])==14
   if i>=3:samples.append(ms)
 p95=sorted(samples)[math.ceil(len(samples)*0.95)-1]
 report={'binary':str(a.binary.resolve()),'bytes':len(data),'mib':len(data)/(1024*1024),'sha256':hashlib.sha256(data).hexdigest(),'samples':len(samples),'median_ms':statistics.median(samples),'p95_ms':p95,'max_ms':max(samples),'conditions':'Fresh process per invocation; warmed OS filesystem cache; no network; three warmups excluded','startup_target_ms':50,'startup_target_met':p95<=50,'size_target_mib':30,'size_target_met':len(data)<=30*1024*1024}
 a.out.write_text(json.dumps(report,indent=2),encoding='utf-8');print(json.dumps(report));return 0 if report['startup_target_met'] and report['size_target_met'] else 1
if __name__=='__main__':raise SystemExit(main())
