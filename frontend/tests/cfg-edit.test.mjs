// cfg-edit.js: Form edits write a value of the field's own type, in a quoting style that the real loader reads back
// as exactly what was typed, and change nothing else in the file.
import test from 'node:test';
import assert from 'node:assert/strict';
import { parseConfig, entryOf } from '../js/cfg-parse.js';
import { encodeValue, spliceValue, spliceBlock, displayValue, flattenFields, yamlPlainSafe, pathKey } from '../js/cfg-edit.js';

const nodeAt = (fmt, text, path) => path.reduce((n, k) => (typeof k === 'number' ? n.items[k] : entryOf(n, k).node), parseConfig(fmt, text).root);
/** Edit one value and read the file back with the same parser: the new value, and every other value untouched. */
function edit(fmt, text, path, input, spec) {
  const n = nodeAt(fmt, text, path);
  const enc = encodeValue(fmt, n, input, spec);
  if (enc.error) return { error: enc.error };
  const out = enc.block !== undefined ? spliceBlock(text, n, enc.block) : spliceValue(text, n, enc.raw);
  const before = flattenFields(parseConfig(fmt, text).root),
    after = flattenFields(parseConfig(fmt, out).root);
  assert.equal(after.length, before.length);
  for (const [i, f] of after.entries()) if (pathKey(f.path) !== pathKey(path)) assert.equal(f.node.raw, before[i].node.raw, `other value changed: ${pathKey(f.path)}`);
  return { out, node: nodeAt(fmt, out, path) };
}

test('YAML strings: plain when safe, quoted when PyYAML would read something else', () => {
  const T = 'a: plain  # keep me\nb: \'single\'\nc: "double"\n';
  assert.equal(edit('yaml', T, ['a'], 'other').out, 'a: other  # keep me\nb: \'single\'\nc: "double"\n');
  for (const tricky of ['yes', 'null', '123', '1e3', 'a: b', 'x #y', '#lead', ' lead', '- dash', '@at', '', '2026-11-04', '[x]', 'tab\tin']) {
    const r = edit('yaml', T, ['a'], tricky);
    assert.equal(r.node.type, 'string', tricky);
    assert.equal(r.node.value, tricky, tricky);
  }
  assert.equal(edit('yaml', T, ['b'], "it's").out.split('\n')[1], "b: 'it''s'");
  assert.equal(edit('yaml', T, ['c'], 'x"y').out.split('\n')[2], 'c: "x\\"y"');
  assert.equal(edit('yaml', 'k: [a, b]\n', ['k', 1], 'c, d').node.value, 'c, d'); // inside [ ]: quoted
  assert.ok(yamlPlainSafe('aitop/nemotron-lightning'));
  assert.ok(!yamlPlainSafe('on'));
  assert.ok(!yamlPlainSafe('x: y'));
});

test('numbers keep their kind; a value of the wrong kind is refused, not written', () => {
  const T = 'i: 4\nf: 0.5\n';
  assert.equal(edit('yaml', T, ['i'], '64').node.value, 64);
  assert.match(edit('yaml', T, ['i'], 'abc').error, /'abc' is not a whole number/);
  assert.match(edit('yaml', T, ['i'], '2.0').error, /without a decimal point/);
  assert.match(edit('yaml', T, ['i'], '0777').error, /not a whole number/); // would be octal
  assert.equal(edit('yaml', T, ['f'], '1').out, 'i: 4\nf: 1.0\n'); // stays a float
  assert.equal(edit('yaml', T, ['f'], '1e-3').node.value, 0.001); // rewritten so PyYAML reads a number
  assert.equal(edit('yaml', T, ['f'], '2e3').node.type, 'float');
  assert.equal(edit('yaml', T, ['i'], '0.25', { t: 'num' }).node.value, 0.25); // the schema allows any number
  assert.equal(edit('toml', 'n = 3\nx = 1.5\n', ['n'], '1_000').node.value, 1000);
  assert.match(edit('toml', 'n = 3\n', ['n'], '012').error, /not a whole number/);
});

test('booleans keep their spelling; empty values', () => {
  assert.equal(edit('yaml', 'a: yes\nb: True\nc: ON\n', ['a'], false).out.split('\n')[0], 'a: no');
  assert.equal(edit('yaml', 'a: yes\nb: True\nc: ON\n', ['b'], false).out.split('\n')[1], 'b: False');
  assert.equal(edit('yaml', 'a: yes\nb: True\nc: ON\n', ['c'], false).out.split('\n')[2], 'c: OFF');
  assert.equal(edit('toml', 'x = true\n', ['x'], false).out, 'x = false\n');
  assert.match(encodeValue('yaml', nodeAt('yaml', 'n: 1\n', ['n']), true).error, /not a true\/false field/);
  assert.equal(edit('yaml', 'e:\nz: 1\n', ['e'], 'text').out, 'e: text\nz: 1\n'); // the space is added
  assert.equal(edit('yaml', 'e:\nz: 1\n', ['e'], '').out, 'e:\nz: 1\n'); // still empty
  assert.equal(edit('yaml', 'e: ~\n', ['e'], '5', { t: 'int' }).node.value, 5);
});

test('TOML strings keep their style when they can', () => {
  assert.equal(edit('toml', "p = 'C:\\x'\n", ['p'], 'D:\\y').out, "p = 'D:\\y'\n");
  assert.equal(edit('toml', "p = 'x'\n", ['p'], "it's").out, 'p = "it\'s"\n');
  assert.equal(edit('toml', 's = "a"  # c\n', ['s'], 'tab\t"q"').out, 's = "tab\\t\\"q\\""  # c\n');
  const ml = edit('toml', 'm = """\nold\n"""\n', ['m'], 'new\ntext\n');
  assert.equal(ml.node.value, 'new\ntext\n');
});

test('YAML block scalars: the lines are re-indented under the key, the header is kept', () => {
  const T = 'prompt: |\n    Hello\n    world\nnext: 1\n';
  const n = nodeAt('yaml', T, ['prompt']);
  assert.equal(displayValue(n), 'Hello\nworld');
  const r = edit('yaml', T, ['prompt'], 'Hi\n\nthere');
  assert.equal(r.out, 'prompt: |\n    Hi\n\n    there\nnext: 1\n');
  assert.equal(r.node.value, 'Hi\n\nthere\n');
});

test('flattenFields lists every scalar with its path, in document order', () => {
  const f = flattenFields(parseConfig('yaml', 'a: 1\nb:\n  - x\n  - k: v\n').root);
  assert.deepEqual(
    f.map((x) => x.path),
    [['a'], ['b', 0], ['b', 1, 'k']],
  );
});

test('0.9.0 audit: values PyYAML would read differently are escaped, refused or normalised', () => {
  const q = edit('yaml', 'a: "x"\n', ['a'], 'a\u0085b\u2028c\u2029d');
  assert.equal(q.node.value, 'a\u0085b\u2028c\u2029d'); // escaped, not folded to spaces
  assert.match(q.out, /\\u0085/);
  assert.equal(edit('yaml', "a: 'x'\n", ['a'], 'p\u2028q').node.value, 'p\u2028q'); // single quotes cannot carry it
  assert.equal(edit('yaml', 'a: x\n', ['a'], '=').out, 'a: "="\n'); // a bare = is the YAML value tag
  assert.equal(edit('yaml', 'a: 0.5\n', ['a'], '-.5').node.value, -0.5); // -.5 is text to PyYAML
  assert.equal(edit('yaml', 'a: x\n', ['a'], '<<').out, 'a: "<<"\n'); // the merge key tag
  assert.match(edit('yaml', 'a: 0.5\n', ['a'], '-.').error, /not a number/); // a half-typed number is not -0.
  assert.match(edit('yaml', 'p: |\n  x\nz: 1\n', ['p'], '  indented\nnot').error, /first line/);
  assert.equal(edit('yaml', 'p: |\n  x\nz: 1\n', ['p'], '\nfirst\n  second').node.value, '\nfirst\n  second\n');
});
