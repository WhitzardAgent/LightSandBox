import unittest

from calc import square


class TestCalc(unittest.TestCase):
    def test_square(self):
        self.assertEqual(square(3), 9)


if __name__ == '__main__':
    unittest.main()
