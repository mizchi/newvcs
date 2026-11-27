import unittest

import strkit


class User:
    def __init__(self, name):
        self.name = name


class Templates(unittest.TestCase):
    def test_names(self):
        self.assertEqual(strkit.render("Hello {name}!", {"name": "Ann"}), "Hello Ann!")
        self.assertEqual(strkit.render("{n} items", {"n": 3}), "3 items")
        self.assertEqual(strkit.render("{a.b.c}", {"a": {"b": {"c": "deep"}}}), "deep")
        self.assertEqual(strkit.render("{user.name}", {"user": User("Bo")}), "Bo")
        self.assertEqual(strkit.render("no fields", {}), "no fields")

    def test_filters(self):
        self.assertEqual(strkit.render("{x|upper}", {"x": "ab"}), "AB")
        self.assertEqual(strkit.render("{x|lower}", {"x": "AB"}), "ab")
        self.assertEqual(strkit.render("{x|title}", {"x": "the end"}), "The End")
        self.assertEqual(strkit.render("[{x|strip}]", {"x": "  a  "}), "[a]")
        self.assertEqual(strkit.render("{x|strip|upper}", {"x": " a "}), "A")

    def test_default(self):
        self.assertEqual(strkit.render("{x|default:none}", {}), "none")
        self.assertEqual(strkit.render("{x|default:none}", {"x": None}), "none")
        self.assertEqual(strkit.render("{x|default:none}", {"x": "set"}), "set")
        self.assertEqual(strkit.render("[{x|default:}]", {}), "[]")
        self.assertEqual(strkit.render("{x|default:anon|upper}", {}), "ANON")
        self.assertEqual(strkit.render("{u.name|default:?}", {"u": {}}), "?")

    def test_braces(self):
        self.assertEqual(strkit.render("{{x}} is {x}", {"x": 1}), "{x} is 1")
        self.assertEqual(strkit.render("}}{{", {}), "}{")

    def test_refuses(self):
        for template, context in [("{x}", {}), ("{x|nope}", {"x": 1}), ("{}", {}), ("{x", {"x": 1}), ("a } b", {}), ("{u.missing}", {"u": User("a")})]:
            with self.assertRaises(strkit.TemplateError, msg=template):
                strkit.render(template, context)

    def test_error(self):
        self.assertTrue(issubclass(strkit.TemplateError, strkit.StrkitError))
