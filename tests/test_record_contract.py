"""Recorder regression: every action must reach validation with non-empty required inputs."""
import importlib.util
import json
from pathlib import Path
import unittest
ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('record_contract',ROOT/'scripts'/'record_contract.py')
recorder=importlib.util.module_from_spec(spec)
spec.loader.exec_module(recorder)
class RecorderInputs(unittest.TestCase):
 def test_required_inputs_are_never_left_unset(self):
  registry=json.loads((ROOT/'tests/golden/registry.json').read_text(encoding='utf-8'))
  for channel in registry['channels']:
   for action in channel['actions']:
    with self.subTest(channel=channel['name'],action=action['name']):
     _,params,_=recorder._action_args(channel,action,action)
     for p in action['params']:
      if p['required']:self.assertNotIn(params.get(p['name']),(None,''),f"{channel['name']}:{action['name']} missing {p['name']}")
if __name__=='__main__':unittest.main()
