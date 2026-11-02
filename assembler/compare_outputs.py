#!/usr/bin/env python3
"""Create a self-contained HTML and JSON comparison of the native build outputs."""

import argparse
import base64
from datetime import datetime, timezone
import json
import math
from pathlib import Path
import re
import struct

from compare_object import compare
from link import rom_region


def coff_sections(path):
    data = path.read_bytes()
    magic, count, _, _, _, optional, _ = struct.unpack_from('<HHIIIHH', data)
    if magic != 0x90:
        raise ValueError(f'{path}: expected TI COFF v0')
    sections = {}
    for index in range(count):
        name, address, _, bits, pointer, _, _, _, _, flags = struct.unpack_from(
            '<8sIIIIIIHHI', data, 20 + optional + index * 40)
        name = name.rstrip(b'\0').decode('ascii')
        sections[name] = dict(bits=bits, address=address, initialized=bool(pointer),
                              payload=data[pointer:pointer + bits // 8] if pointer else b'')
    return sections


def byte_comparison(actual, expected):
    common = min(len(actual), len(expected))
    differences = [index for index in range(common) if actual[index] != expected[index]]
    first = differences[0] if differences else (common if len(actual) != len(expected) else None)
    length = max(len(actual), len(expected))
    bucket_size = max(1, math.ceil(length / 256))
    buckets = []
    for start in range(0, length, bucket_size):
        end = min(start + bucket_size, length)
        count = sum(actual[index] != expected[index] for index in range(start, min(end, common)))
        count += max(0, end - max(start, common))
        buckets.append(dict(start=start, end=end, differences=count, ratio=count / (end - start)))
    return dict(differing_bytes=len(differences) + abs(len(actual) - len(expected)),
                first_difference=first, buckets=buckets)


def collect(build_dir, legacy_dir, cmd_file):
    map_text = (legacy_dir / 'BB.MAP').read_text(errors='replace')
    expected_totals = {}
    map_inputs = {}
    for line in map_text.splitlines():
        fields = line.split()
        if len(fields) >= 4 and fields[0] in ('.text', '.data', '.bss') and fields[1].isdigit():
            expected_totals[fields[0]] = int(fields[3], 16)
        match = re.match(r'^\s+[0-9a-f]{8}\s+([0-9a-f]{8})\s+(\S+\.OBJ)\s+\(([^)]+)\)', line, re.I)
        if match:
            map_inputs[(match[2].upper(), match[3])] = int(match[1], 16)
    # Use the declared production modules, including missing outputs. Diagnostic
    # objects elsewhere in build/ must never inflate these totals.
    script = Path(__file__).with_name('build.ps1').read_text()
    module_block = re.search(r'\$modules\s*=\s*@\((.*?)\)', script, re.S).group(1)
    modules = re.findall(r'"([A-Za-z0-9_]+)"', module_block)
    rows, totals = [], {name: 0 for name in expected_totals}
    coverage = []
    for module in modules:
        name = module + '.OBJ'
        path, reference = build_dir / name, legacy_dir / name
        native = json.loads(path.read_text()) if path.exists() else None
        coverage.append(dict(module=module, omitted=native.get('omitted_instructions') if native else None))
        payloads = {s['name'].lower(): len(base64.b64decode(s['data'])) * 8
                    for s in native['sections']} if native else {}
        legacy = coff_sections(reference) if reference.exists() else {}
        diagnostic = {s['section']: s for s in compare(path, reference)['sections']} if native and legacy else {}
        for section in ('.text', '.data', '.bss'):
            actual = (native.get('bss_size_bits', 0) if section == '.bss' else payloads.get(section, 0)) if native else None
            expected = map_inputs.get((name.upper(), section), legacy.get(section, {}).get('bits'))
            if actual is not None:
                totals[section] = totals.get(section, 0) + actual
            rows.append(dict(module=module, section=section, actual_bits=actual, expected_bits=expected,
                             comparison=diagnostic.get(section),
                             status='missing' if native is None else 'no reference' if expected is None else
                             'same size' if actual == expected else 'size differs'))
    artifacts = []
    _, region_bits = rom_region(cmd_file)
    rom_reference = legacy_dir / 'ROM' / 'ROM_MINE.bin'
    for relative in ['BB.OUT', 'BB.bin', 'ROM/ROM_MINE.bin',
                     'ROM/l1.03_maximum_hangtime_u54_l_version.u54',
                     'ROM/l1.03_maximum_hangtime_u63_l_version.u63']:
        path = build_dir / relative
        reference = legacy_dir / relative
        expected = reference.read_bytes() if reference.exists() else None
        note = ''
        if relative == 'BB.bin':
            expected = rom_reference.read_bytes()[:region_bits // 8] if rom_reference.exists() else None
            note = 'Compared with the reference ROM prefix covering BB.CMD ROM memory.'
        elif relative == 'BB.OUT':
            note = 'Raw container bytes include metadata and symbols; compare linked sections below for payloads.'
        actual = path.read_bytes() if path.exists() else None
        artifacts.append(dict(name=relative, actual_bytes=len(actual) if actual is not None else None,
                              expected_bytes=len(expected) if expected is not None else None, note=note,
                              comparison=byte_comparison(actual, expected) if actual is not None and expected is not None else None))
    linked = []
    actual_coff = coff_sections(build_dir / 'BB.OUT') if (build_dir / 'BB.OUT').exists() else {}
    legacy_coff = coff_sections(legacy_dir / 'BB.OUT')
    for name in dict.fromkeys([*legacy_coff, *actual_coff]):
        actual, expected = actual_coff.get(name), legacy_coff.get(name)
        linked.append(dict(name=name, actual_bits=actual['bits'] if actual else None,
                           expected_bits=expected['bits'] if expected else None,
                           actual_address=actual['address'] if actual else None,
                           expected_address=expected['address'] if expected else None,
                           comparison=byte_comparison(actual['payload'], expected['payload'])
                           if actual and expected and (actual['initialized'] or expected['initialized']) else None))
    return dict(generated=datetime.now(timezone.utc).isoformat(), build=str(build_dir.resolve()),
                reference=str(legacy_dir.resolve()), modules=rows, coverage=coverage, artifacts=artifacts, linked_sections=linked,
                totals=[dict(section=s, actual_bits=totals.get(s, 0), expected_bits=bits) for s, bits in expected_totals.items()])


def render(report):
    # No external assets: the report can be opened directly from disk offline.
    data = json.dumps(report).replace('<', '\\u003c')
    return '''<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Native build comparison</title><style>
*{box-sizing:border-box}body{margin:0;background:#101721;color:#e5edf7;font:15px system-ui,sans-serif}main{max-width:1500px;margin:auto;padding:32px}h1{font-size:32px;margin:0}h2{margin-top:36px}p{color:#aebed1;line-height:1.6}.cards{display:flex;gap:14px;flex-wrap:wrap}.card{background:#1a2635;padding:18px;border-radius:10px;flex:1;min-width:220px}.big{font-size:25px;font-weight:700;margin:8px 0}table{border-collapse:collapse;width:100%;background:#162130}th,td{padding:10px 12px;border-bottom:1px solid #2a394b;text-align:left;vertical-align:top}th{color:#b9cbe1;cursor:pointer;white-space:nowrap}td.num{font-variant-numeric:tabular-nums;white-space:nowrap}.scroll{overflow:auto}.bad{color:#ffac91}.good{color:#7cdbc2}.muted{color:#a3b3c6;font-size:13px}.bar{width:180px;max-width:100%;background:#334052;height:7px;margin:4px 0;border-radius:3px;overflow:hidden}.bar i{display:block;height:100%;background:#64b8ed}.bar i.ref{background:#8190a5}.heat{display:grid;grid-template-columns:repeat(64,1fr);gap:1px;margin:12px 0}.heat i{height:8px}input{background:#213147;border:1px solid #42566f;color:white;padding:9px;border-radius:5px}label{display:inline-block;margin:12px 18px 12px 0}code{color:#b6d9ff}details{margin:6px 0}.pill{background:#30415a;padding:3px 8px;border-radius:20px;font-size:12px}a{color:#8ccaff}@media(max-width:700px){main{padding:16px}.heat{grid-template-columns:repeat(32,1fr)}}
</style><main><h1>Native build comparison</h1><p id="context"></p>
<p>Legacy sizes are reference targets, not proof that the current source should produce identical bytes. Source changes and unaudited opcodes remain. All sizes below are bytes; addresses are TMS bit addresses. Equal size does not establish byte equality.</p>
<div class="cards" id="totals"></div><h2>Instruction emission coverage</h2><div id="coverage"></div><p>These counts identify mnemonics accepted by the parser but emitting no bytes. They do not check the correctness of emitted opcodes or detect every parser issue. Link success alone does not establish executable correctness.</p><h2>Linked outputs</h2><p>Green heatmap cells match; warmer cells contain more differing bytes. Hover for byte ranges. Missing outputs and absent references are shown explicitly.</p><div id="artifacts"></div>
<h2>COFF sections</h2><p>The BSS header size in BB.OUT is currently fixed by the writer. Use the object BSS total above to assess actual allocation sizes.</p><div class="scroll"><table id="linked"></table></div>
<h2>All module contributions</h2><p>Bars compare native (blue) and legacy (gray) section sizes. Size targets come from BB.MAP, falling back to preserved object headers. Byte checks use the preserved objects. Differences compare common offsets outside legacy relocation fields; differing layouts or inputs can shift those bytes. Unsupported relocation kinds are marked unavailable. Relocated target values are not compared. Source-baseline findings are documented in handoff.md; this report measures differences and cannot establish their causes.</p>
<label>Find module <input id="search" placeholder="e.g. PAL, BB3"></label><label><input type="checkbox" id="different"> Differences or unavailable comparisons only</label><span id="count" class="pill"></span>
<div class="scroll"><table id="modules"></table></div><p class="muted">Click module table headings to sort. Refresh by rerunning compare_outputs.py. The companion JSON contains exact bit counts and heatmap ranges.</p></main>
<script>const R=REPORT_DATA;
const esc=x=>String(x??'unavailable').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const fmt=x=>x==null?'unavailable':Number(x).toLocaleString(undefined,{maximumFractionDigits:3});
const bytes=x=>x==null?null:x/8;
const delta=(a,b)=>a==null||b==null?'unavailable':(a>b?'+':'')+fmt(a-b);
const addr=x=>x==null?'unavailable':'0x'+x.toString(16).toUpperCase();
const bars=(a,b)=>{const m=Math.max(a||0,b||0,1);return `<div class="bar" title="Native"><i style="width:${100*(a||0)/m}%"></i></div><div class="bar" title="Legacy"><i class="ref" style="width:${100*(b||0)/m}%"></i></div>`};
document.querySelector('#context').textContent=`Generated ${R.generated} · Native: ${R.build} · Reference: ${R.reference}`;
document.querySelector('#totals').innerHTML=R.totals.map(x=>`<div class="card"><div>${esc(x.section)}</div><div class="big">${fmt(bytes(x.actual_bits))} B</div><div>Legacy ${fmt(bytes(x.expected_bits))} B</div><p class="${x.actual_bits===x.expected_bits?'good':'bad'}">${delta(bytes(x.actual_bits),bytes(x.expected_bits))} B</p>${bars(x.actual_bits,x.expected_bits)}</div>`).join('');
const coverage=R.coverage||[];const omitted=coverage.filter(x=>x.omitted?.length);const unknown=coverage.filter(x=>x.omitted==null).length;const omissions=omitted.flatMap(x=>x.omitted).reduce((sum,x)=>sum+x.count,0);
document.querySelector('#coverage').innerHTML=`<p class="${omissions||unknown?'bad':'good'}">${fmt(omissions)} omitted instruction occurrences across ${omitted.length} modules. Coverage metadata unavailable for ${unknown} modules.</p>`+(omitted.length?'<div class="scroll"><table><tr><th>Module</th><th>Omitted mnemonic (count)</th></tr>'+omitted.map(x=>`<tr><td>${esc(x.module)}</td><td>${x.omitted.map(i=>`${esc(i.mnemonic)} (${fmt(i.count)})`).join(', ')}</td></tr>`).join('')+'</table></div>':'');
document.querySelector('#artifacts').innerHTML=R.artifacts.map(x=>{const c=x.comparison;return `<div class="card" style="margin:12px 0"><strong>${esc(x.name)}</strong><div>Native ${fmt(x.actual_bytes)} B · Legacy ${fmt(x.expected_bytes)} B · Delta ${delta(x.actual_bytes,x.expected_bytes)} B</div><div class="${c&&c.differing_bytes===0?'good':'bad'}">${c?fmt(c.differing_bytes)+' differing bytes · first '+addr(c.first_difference):'Missing output or reference'}</div><div class="heat">${(c?.buckets||[]).map(b=>`<i title="${addr(b.start)}–${addr(b.end-1)}: ${b.differences}/${b.end-b.start} bytes differ" style="background:${b.ratio===0?'#368875':`hsl(${38*(1-b.ratio)},80%,${34+20*b.ratio}%)`}"></i>`).join('')}</div><span class="muted">${esc(x.note)}</span></div>`}).join('');
document.querySelector('#linked').innerHTML='<tr><th>Section</th><th>Native B</th><th>Legacy B</th><th>Delta B</th><th>Native / legacy address</th><th>Payload differing bytes</th></tr>'+R.linked_sections.map(x=>`<tr><td>${esc(x.name)}</td><td>${fmt(bytes(x.actual_bits))}</td><td>${fmt(bytes(x.expected_bits))}</td><td class="${x.actual_bits===x.expected_bits?'good':'bad'}">${delta(bytes(x.actual_bits),bytes(x.expected_bits))}</td><td>${addr(x.actual_address)} / ${addr(x.expected_address)}</td><td>${x.comparison?fmt(x.comparison.differing_bytes):'No initialized payload'}</td></tr>`).join('');
let key='module',direction=1;
function rows(){let list=R.modules.filter(x=>x.module.toLowerCase().includes(document.querySelector('#search').value.toLowerCase()));if(document.querySelector('#different').checked)list=list.filter(x=>x.status!=='same size'||(x.section!=='.bss'&&(!x.comparison?.size_and_non_relocated_bytes_match||!x.comparison?.relocation_sites_match)));
list.sort((a,b)=>{let av=key==='delta'?(a.actual_bits??0)-(a.expected_bits??0):a[key],bv=key==='delta'?(b.actual_bits??0)-(b.expected_bits??0):b[key];return (typeof av==='number'&&typeof bv==='number'?av-bv:String(av).localeCompare(String(bv)))*direction});
document.querySelector('#count').textContent=`${list.length} / ${R.modules.length} contributions`;
document.querySelector('#modules').innerHTML='<tr>'+[['module','Module'],['section','Section'],['actual_bits','Native B'],['expected_bits','Legacy B'],['delta','Delta B'],['status','Size status']].map(([k,t])=>`<th data-key="${k}">${t}</th>`).join('')+'<th>Relative size</th><th>Byte / relocation comparison</th></tr>'+list.map(x=>{const c=x.comparison;let note=x.section==='.bss'?'Uninitialized allocation':!c?'No comparable object':c.unsupported_legacy_relocation_types.length?'Unavailable: relocation types '+c.unsupported_legacy_relocation_types.map(addr).join(', '):`${fmt(c.non_relocated_common_byte_differences)} byte differences; ${c.relocation_sites_match?'matching':'different'} relocation sites`;return `<tr><td>${esc(x.module)}</td><td>${esc(x.section)}</td><td class="num">${fmt(bytes(x.actual_bits))}</td><td class="num">${fmt(bytes(x.expected_bits))}</td><td class="num ${x.status==='same size'?'good':'bad'}">${delta(bytes(x.actual_bits),bytes(x.expected_bits))}</td><td>${esc(x.status)}</td><td>${bars(x.actual_bits,x.expected_bits)}</td><td>${esc(note)}</td></tr>`}).join('');document.querySelectorAll('th[data-key]').forEach(th=>th.onclick=()=>{direction=key===th.dataset.key?-direction:1;key=th.dataset.key;rows()});}
document.querySelector('#search').oninput=rows;document.querySelector('#different').onchange=rows;rows();
</script></html>'''.replace('REPORT_DATA', data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    root = Path(__file__).resolve().parent
    parser.add_argument('--build-dir', type=Path, default=root / 'build')
    parser.add_argument('--legacy-dir', type=Path, default=root.parent / 'BACKUP' / 'XCODE101L')
    parser.add_argument('--cmd-file', type=Path, default=root.parent / 'SRC' / 'BB.CMD')
    parser.add_argument('--output', type=Path, default=root / 'build' / 'comparison.html')
    args = parser.parse_args()
    report = collect(args.build_dir, args.legacy_dir, args.cmd_file)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(render(report), encoding='utf-8')
    args.output.with_suffix('.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
    print(f'Wrote {args.output} and {args.output.with_suffix(".json")}')


if __name__ == '__main__':
    main()
