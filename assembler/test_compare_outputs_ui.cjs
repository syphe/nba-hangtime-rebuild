// Exercise the generated report's DOM updates without a browser or dependencies.
const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const page = fs.readFileSync(process.argv[2] || 'build/comparison.html', 'utf8');
const nodes = {};
const document = {
  querySelector: key => nodes[key] ||= { innerHTML: '', textContent: '', value: '', checked: false },
  querySelectorAll: () => [],
};
const context = vm.createContext({ document });
const script = page.match(/<script>([\s\S]*)<\/script>/)[1];
vm.runInContext(script, context);
assert.match(nodes['#count'].textContent, /^153 \/ 153/);
assert.equal((nodes['#modules'].innerHTML.match(/<tr>/g) || []).length, 154);
nodes['#search'].value = 'PAL';
nodes['#search'].oninput();
assert.match(nodes['#count'].textContent, /^9 \/ 153/);
nodes['#search'].value = 'TEXT';
nodes['#search'].oninput();
assert.match(nodes['#count'].textContent, /^3 \/ 153/);
nodes['#different'].checked = true;
nodes['#different'].onchange();
assert.match(nodes['#count'].textContent, /^0 \/ 153/);
nodes['#search'].value = '';
nodes['#different'].checked = false;
const fixture = vm.runInContext('JSON.parse(JSON.stringify(R))', context);
fixture.modules = [{module:'ZZZ',section:'.text',actual_bits:128,expected_bits:64,status:'size differs',comparison:null},
                   {module:'AAA',section:'.text',actual_bits:0,expected_bits:64,status:'size differs',comparison:null}];
fixture.coverage = [{module:'DEMO',omitted:[{mnemonic:'SEXT',count:2},{mnemonic:'NEG',count:1}]},
                    {module:'OLD',omitted:null}];
const coverageNodes = {};
const coverageDocument = { querySelector: key => coverageNodes[key] ||= {innerHTML:'',value:'',checked:false},
                           querySelectorAll: () => [] };
const fixtureScript = script.replace(/^const R=.*;\r?\n/, 'const R='+JSON.stringify(fixture)+';\n');
assert.notEqual(fixtureScript, script);
const fixtureContext = vm.createContext({document:coverageDocument});
vm.runInContext(fixtureScript, fixtureContext);
vm.runInContext("key='delta';direction=-1;rows()", fixtureContext);
assert.match(coverageNodes['#modules'].innerHTML, /<tr><td>ZZZ<\/td>/);
assert.match(coverageNodes['#coverage'].innerHTML, /3 omitted instruction occurrences across 1 modules/);
assert.match(coverageNodes['#coverage'].innerHTML, /unavailable for 1 modules/);
assert.match(coverageNodes['#coverage'].innerHTML, /SEXT \(2\), NEG \(1\)/);
console.log('PASS: report renders 153 contributions; search, filtering, sorting, and coverage counts work.');
