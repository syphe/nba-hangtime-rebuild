import unittest

from compare_outputs import byte_comparison, render


class OutputComparisonTests(unittest.TestCase):
    def test_differences_and_heatmap_include_missing_tail_bytes(self):
        result = byte_comparison(b'abcX', b'abYdef')
        self.assertEqual(result['differing_bytes'], 4)
        self.assertEqual(result['first_difference'], 2)
        self.assertEqual(sum(bucket['differences'] for bucket in result['buckets']), 4)
        for actual, expected in [(b'ab', b'abcd'), (b'abcd', b'ab')]:
            self.assertEqual(byte_comparison(actual, expected)['first_difference'], 2)

    def test_equal_empty_and_large_images_have_consistent_buckets(self):
        self.assertEqual(byte_comparison(b'', b'')['buckets'], [])
        self.assertIsNone(byte_comparison(b'abc', b'abc')['first_difference'])
        result = byte_comparison(bytes(1001), bytes([1]) * 1001)
        self.assertLessEqual(len(result['buckets']), 256)
        self.assertEqual(sum(bucket['differences'] for bucket in result['buckets']), 1001)

    def test_embedded_report_cannot_close_its_script_element(self):
        report = render({'name': '</script><script>bad()</script>'})
        self.assertNotIn('</script><script>bad()', report)
        self.assertIn('\\u003c/script>', report)


if __name__ == '__main__':
    unittest.main()
