import base64
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest

from link import relative_fixup_bytes


class RelativeRelocations(unittest.TestCase):
    def test_move_address_and_offset_fields_relocate_without_touching_opcodes(self):
        code = struct.pack('<HIHIHIIHH', 0x07a0,0,0x0780,0,0x07c0,0,0,0xb622,0)
        obj = dict(format='tms34010-object', sections=[
            dict(name='.text',data=base64.b64encode(code).decode()),
            dict(name='.data',data=base64.b64encode(bytes(4)).decode())],
            symbols=[dict(name='src',section='.data',section_offset=0,kind='symbol'),
                     dict(name='dst',section='.data',section_offset=2,kind='symbol'),
                     dict(name='FIELD',section='',address=32,kind='symbol')],
            fixups=[dict(section='.text',offset=offset,symbol=expression,kind=kind) for offset,expression,kind in
                    [(2,'src+16','absolute32'),(8,'dst','absolute32'),(14,'src','absolute32'),
                     (18,'dst','absolute32'),(24,'FIELD+16','absolute16')]])
        build = Path(__file__).resolve().parent / 'build'
        build.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=build) as temporary:
            root = Path(temporary)
            self.assertTrue(root.resolve().is_relative_to(build.resolve()))
            (root/'test.OBJ').write_text(json.dumps(obj))
            subprocess.run([sys.executable,str(Path(__file__).with_name('link.py')),str(root/'test.OBJ'),
                            '--output',str(root/'ROM.bin')],check=True,capture_output=True)
            source = 0xff800000 + len(code)*8
            expected = struct.pack('<HIHIHIIHH',0x07a0,source+16,0x0780,source+16,0x07c0,source,source+16,0xb622,48)
            self.assertEqual((root/'ROM.bin').read_bytes()[:len(code)],expected)

    def test_movb_two_displacements_relocate_in_source_destination_order(self):
        code = struct.pack('<HHH', 0xbc53, 0, 0)
        obj = dict(format='tms34010-object', sections=[
            dict(name='.text',data=base64.b64encode(code).decode())],
            symbols=[dict(name='FIELD',section='',address=32,kind='symbol')],
            fixups=[dict(section='.text',offset=offset,symbol=expression,kind='absolute16')
                    for offset,expression in [(2,'FIELD+16'),(4,'FIELD-16')]])
        build = Path(__file__).resolve().parent / 'build'
        build.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=build) as temporary:
            root = Path(temporary)
            self.assertTrue(root.resolve().is_relative_to(build.resolve()))
            (root/'test.OBJ').write_text(json.dumps(obj))
            subprocess.run([sys.executable,str(Path(__file__).with_name('link.py')),str(root/'test.OBJ'),
                            '--output',str(root/'ROM.bin')],check=True,capture_output=True)
            self.assertEqual((root/'ROM.bin').read_bytes()[:len(code)],struct.pack('<HHH',0xbc53,48,16))

    def test_short_branches_patch_low_byte_and_preserve_condition(self):
        for displacement in (-127, -1, 1, 127):
            self.assertEqual(relative_fixup_bytes('tms34010-relative', 0x1000,
                             0x1010 + displacement * 16, 0xCA55),
                             bytes([displacement & 255, 0xCA]))
        for displacement in (-128, 0, 128):
            with self.assertRaises(ValueError):
                relative_fixup_bytes('tms34010-relative', 0, 16 + displacement * 16, 0xCA00)

    def test_long_and_dsjs_limits_and_alignment(self):
        for displacement in (-32768, 0, 32767):
            self.assertEqual(relative_fixup_bytes('tms34010-relative16', 0x100000,
                             0x100010 + displacement * 16, 0), struct.pack('<h', displacement))
        for displacement, expected in [(-31, 0x3FF3), (-1, 0x3C33), (0, 0x3813), (31, 0x3BF3)]:
            self.assertEqual(relative_fixup_bytes('tms34010-dsjs5', 0x1000,
                             0x1010 + displacement * 16, 0x3C33), struct.pack('<H', expected))
        for kind in ('tms34010-relative', 'tms34010-relative16', 'tms34010-dsjs5'):
            with self.assertRaises(ValueError):
                relative_fixup_bytes(kind, 0x1000, 0x1011, 0)
        for displacement in (-32, 32):
            with self.assertRaises(ValueError):
                relative_fixup_bytes('tms34010-dsjs5', 0x1000, 0x1010 + displacement * 16, 0x3813)

    def test_linker_applies_short_and_dsjs_fields_in_real_rom(self):
        obj = dict(format='tms34010-object', sections=[dict(name='.text', data=base64.b64encode(
            bytes([0x55, 0xCA, 0x33, 0x3C, 0, 3, 0, 3])).decode())],
            symbols=[dict(name='#local', section='.text', section_offset=6, kind='symbol')],
            fixups=[dict(section='.text', offset=0, symbol='#local', kind='tms34010-relative'),
                    dict(section='.text', offset=2, symbol='#local', kind='tms34010-dsjs5')])
        build = Path(__file__).resolve().parent / 'build'
        build.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=build) as temporary:
            root = Path(temporary)
            self.assertTrue(root.resolve().is_relative_to(build.resolve()))
            (root / 'test.OBJ').write_text(json.dumps(obj))
            other = json.loads(json.dumps(obj))
            other['sections'][0]['data'] = base64.b64encode(bytes([0x55,0xCB,0x33,0x3C,0,3,0,3])).decode()
            (root / 'other.OBJ').write_text(json.dumps(other))
            subprocess.run([sys.executable, str(Path(__file__).with_name('link.py')),
                            str(root / 'test.OBJ'), str(root / 'other.OBJ'), '--output', str(root / 'ROM.bin')],
                           check=True, capture_output=True)
            self.assertEqual((root / 'ROM.bin').read_bytes()[:4], bytes([2, 0xCA, 0x33, 0x38]))
            self.assertEqual((root / 'ROM.bin').read_bytes()[8:12], bytes([2, 0xCB, 0x33, 0x38]))


if __name__ == '__main__':
    unittest.main()
