import importlib.util
from pathlib import Path
import unittest
ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('verify_live_obscura',ROOT/'scripts/verify_live_obscura.py')
assert spec is not None and spec.loader is not None
obscura=importlib.util.module_from_spec(spec);spec.loader.exec_module(obscura)
class RenderEvidence(unittest.TestCase):
 def test_script_template_is_not_counted_as_a_rendered_dom_node(self):
  source='<html><body><script>var template = \'<div class="quote">fake</div>\';</script></body></html>'
  self.assertEqual(obscura.count_quote_nodes(source),0)
  self.assertEqual(obscura.count_quote_nodes('<div class="quote extra">actual</div>'),1)
if __name__=='__main__':unittest.main()
