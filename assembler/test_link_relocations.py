import base64
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest

from link import relative_fixup_bytes, resolve_expression


class RelativeRelocations(unittest.TestCase):
    def test_ti_expressions_match_preserved_words_and_operator_groups(self):
        for expression, expected in [
            ('4<<10+4<<5+7',0x1087),
            ('[0F2H,166H]-[41H,29H]',0x00b1013d),
            ('[1,-2]',0x1fffe), ('8&3+1',1), ('1|2*4+1',10),
            ('16*3/8',6), ('-7/3',-2), ('-7%3',-1), ("','",44),
            ('(~(outside))&0FFFFFFFFh',0x007fffff),
            ('#after-#before',32),
        ]:
            self.assertEqual(resolve_expression(expression,{'outside':0xff800000,'#after':96,'#before':64}),expected,expression)
        self.assertEqual(resolve_expression('$100',{'$100':0xff800100}),0xff800100)
        self.assertEqual(resolve_expression('$100',{}),0x100)
        for expression in ('1+','2 trailing','(1+2','1/0'):
            with self.assertRaises(ValueError,msg=expression):
                resolve_expression(expression,{})

    def test_long_alu_relocations_preserve_opcodes_and_complement_values(self):
        code=struct.pack('<HIHIHI',0x0b20,0,0x0d13,0,0x0b63,0)
        obj=dict(format='tms34010-object',sections=[dict(name='.text',data=base64.b64encode(code).decode()),
                  dict(name='.data',data=base64.b64encode(bytes(4)).decode())],
                 symbols=[dict(name='target',section='.data',section_offset=0,kind='symbol')],
                 fixups=[dict(section='.text',offset=offset,symbol=expression,kind='absolute32')
                         for offset,expression in [(2,'target+16'),(8,'(~(target))&0FFFFFFFFh'),
                                                   (14,'(~(target+16))&0FFFFFFFFh')]])
        build=Path(__file__).resolve().parent/'build'
        build.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=build) as temporary:
            root=Path(temporary)
            self.assertTrue(root.resolve().is_relative_to(build.resolve()))
            (root/'test.OBJ').write_text(json.dumps(obj))
            subprocess.run([sys.executable,str(Path(__file__).with_name('link.py')),str(root/'test.OBJ'),
                            '--output',str(root/'ROM.bin')],check=True,capture_output=True)
            target=0xff800000+len(code)*8
            expected=struct.pack('<HIHIHI',0x0b20,target+16,0x0d13,(~target)&0xffffffff,
                                 0x0b63,(~(target+16))&0xffffffff)
            self.assertEqual((root/'ROM.bin').read_bytes()[:len(code)],expected)

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
