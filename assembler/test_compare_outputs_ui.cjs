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
vm.runInContext(page.match(/<script>([\s\S]*)<\/script>/)[1], context);
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
vm.runInContext("key='delta';direction=-1;rows()", context);
assert.match(nodes['#modules'].innerHTML, /<tr><td>ATTRACT<\/td><td>\.data<\/td>/);
console.log('PASS: report renders 153 contributions; search, mismatch filtering, and sorting work.');
