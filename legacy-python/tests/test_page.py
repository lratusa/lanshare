import os
import re
import unittest

from lanshare.server import WEB_DIR


class PageTest(unittest.TestCase):
    def setUp(self):
        with open(os.path.join(WEB_DIR, "index.html"), encoding="utf-8") as f:
            self.html = f.read()

    def test_has_all_sections(self):
        for element_id in ["login", "pinInput", "picker", "queue", "textInput", "textList", "fileList", "qr", "pin"]:
            self.assertIn('id="%s"' % element_id, self.html, element_id)

    def test_no_external_resources(self):
        # 断网的局域网也要能用
        self.assertIsNone(re.search(r"""(src|href)\s*=\s*["']?(https?:)?//""", self.html))
        self.assertNotIn("@import", self.html)

    def test_never_renders_html_from_data(self):
        for sink in ["innerHTML", "outerHTML", "insertAdjacentHTML", "document.write"]:
            self.assertNotIn(sink, self.html, sink)

    def test_hidden_attribute_wins_over_display_rules(self):
        self.assertIn("[hidden] { display: none !important; }", self.html)


if __name__ == "__main__":
    unittest.main()
