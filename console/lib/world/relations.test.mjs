// Conformance: the hand-written TypeScript contract decodes the CANONICAL
// fixture that Rust emits, and fails closed on everything it should.
//
//   node --test lib/world/relations.test.mjs
//
// The fixture is `contracts/world_relations_v1.json`, written by
// `crates/kirra-explain-types/tests/relations_contract_fixture.rs`. Neither
// tree owns it. A Rust change that adds, removes, renames or reshapes a field
// moves that file, and this test is what turns the move into a red build here
// instead of a wrong render in production.
//
// The fixture is a LIST of outcome documents, one per variant, because the two
// variants this console must render most carefully are the refusals — a fixture
// built from the happy path would have left `not_an_entity` and `unavailable`
// free to drift, and those are exactly the two that must never be mistaken for
// "related to nothing".

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

const here = dirname(fileURLToPath(import.meta.url))
const FIXTURE = join(here, '../../../contracts/world_relations_v1.json')

// The decoder is TypeScript, so this test reads the source and evaluates the
// parts it needs rather than importing a build artifact. Keeping the test
// dependency-free is deliberate: a conformance check that needs a bundler is a
// conformance check that gets skipped when the bundler breaks.
const SOURCE = readFileSync(join(here, 'relations.ts'), 'utf8')

function loadFixture() {
  const docs = JSON.parse(readFileSync(FIXTURE, 'utf8'))
  assert.ok(Array.isArray(docs), 'the fixture is a list of outcome documents')
  return docs
}

function only(docs, outcome) {
  const found = docs.filter((d) => d.outcome === outcome)
  assert.equal(found.length, 1, `expected exactly one "${outcome}" document, got ${found.length}`)
  return found[0]
}

test('the fixture carries every outcome variant the console switches on', () => {
  const docs = loadFixture()
  const tags = docs.map((d) => d.outcome).sort()
  assert.deepEqual(
    tags,
    ['not_an_entity', 'related', 'unavailable'],
    'a variant missing here is a variant free to drift',
  )

  // Every tag must have a decoder arm. A Rust-side rename would show up as a
  // fixture tag with no `case` for it, which is what this catches.
  for (const tag of tags) {
    assert.ok(
      SOURCE.includes(`case '${tag}':`) || SOURCE.includes(`case '${tag}': {`),
      `the decoder has no arm for outcome "${tag}"`,
    )
  }
})

test('both refusals carry a reason, so neither renders as an empty answer', () => {
  const docs = loadFixture()
  for (const outcome of ['not_an_entity', 'unavailable']) {
    const doc = only(docs, outcome)
    assert.equal(typeof doc.reason, 'string', `${outcome}.reason must be a string`)
    assert.ok(doc.reason.trim().length > 0, `${outcome}.reason must not be empty`)
    // No `view`: a refusal that carried one could be rendered as a relation
    // list, which is the collapse these variants exist to prevent.
    assert.ok(!('view' in doc), `${outcome} must not carry a view`)
  }
})

test('the fixture carries every field the contract declares', () => {
  const doc = only(loadFixture(), 'related')
  assert.ok(Array.isArray(doc.view.related))
  assert.equal(typeof doc.view.subject, 'string')
  assert.equal(typeof doc.view.truncated, 'boolean')
  assert.equal(doc.view.truncated, true, 'truncated must be exercised as true, not merely present')

  // Every field named in the TS interface must be present on every row. This
  // is the half that catches a RENAME: if Rust renames `decision_marker`, the
  // fixture no longer has it and this fails, naming the field.
  const FIELDS = ['low', 'high', 'other', 'adjudicator', 'decision_marker', 'provenance']
  for (const [i, row] of doc.view.related.entries()) {
    for (const f of FIELDS) {
      assert.ok(f in row, `related[${i}] is missing "${f}" — the Rust contract moved`)
    }
    // And no field the console does not know about is silently ignored in the
    // FIXTURE. (The decoder tolerates extras at runtime, deliberately; the
    // fixture is the place to notice one.)
    for (const k of Object.keys(row)) {
      assert.ok(FIELDS.includes(k), `related[${i}] has unknown field "${k}" — decide how it renders`)
    }
  }
})

test('the fixture exercises all four provenance states', () => {
  const doc = only(loadFixture(), 'related')
  const seen = new Set(doc.view.related.map((r) => r.provenance))
  assert.deepEqual(
    [...seen].sort(),
    ['dangling', 'degraded', 'plural', 'resolved'],
    'a fixture missing a state would let that state drift unnoticed',
  )
})

test('every provenance state the fixture carries has a presentation', () => {
  const doc = only(loadFixture(), 'related')
  for (const row of doc.view.related) {
    // The presentation table is the UI's promise that the four stay distinct.
    // Matched out of source rather than imported so this test needs no build.
    const block = SOURCE.slice(SOURCE.indexOf('PROVENANCE_PRESENTATION'))
    assert.ok(
      new RegExp(`\\b${row.provenance}:\\s*\\{`).test(block),
      `no presentation for "${row.provenance}" — a state without one would render as nothing`,
    )
  }
})

test('the four presentations are distinct in TEXT, not only in colour', () => {
  const block = SOURCE.slice(SOURCE.indexOf('PROVENANCE_PRESENTATION'))
  const labels = [...block.matchAll(/label:\s*'([^']+)'/g)].map((m) => m[1])
  const glyphs = [...block.matchAll(/glyph:\s*'([^']+)'/g)].map((m) => m[1])
  assert.equal(labels.length, 4, `expected four labels, got ${labels.length}`)
  assert.equal(new Set(labels).size, 4, `labels collide: ${labels.join(', ')}`)
  assert.equal(new Set(glyphs).size, 4, `glyphs collide: ${glyphs.join(', ')}`)

  // THE LOAD-BEARING ONE: a weaker state may not read as a stronger one. The
  // check is textual and blunt on purpose — it catches the specific collapse
  // that would be easiest to introduce and hardest to notice.
  const degraded = block.slice(block.indexOf('degraded:'))
  assert.ok(
    /compact/i.test(degraded),
    'the degraded presentation must say the evidence was compacted, not merely that it is absent',
  )
  const dangling = block.slice(block.indexOf('dangling:'))
  assert.ok(
    /do not read this as evidence that was successfully observed/i.test(dangling),
    'the dangling presentation must refuse to imply the evidence was observed',
  )
  const plural = block.slice(block.indexOf('plural:'))
  assert.ok(
    /no single one of them is authoritative/i.test(plural),
    'the plural presentation must not present one carrier as authoritative',
  )
})

test('the contract version matches the Rust side', () => {
  const m = SOURCE.match(/RELATIONS_VIEW_VERSION\s*=\s*(\d+)/)
  assert.ok(m, 'the console must declare a contract version')
  assert.equal(m[1], '1', 'a version bump in Rust must be a deliberate act here too')
})

test('the decoder refuses an unknown provenance state rather than falling back', () => {
  // Asserted against the SOURCE because importing TypeScript needs a build.
  // The property under test is a policy statement, and the policy is that no
  // fallback exists: there must be no `?? 'unknown'`, no default arm, and the
  // membership check must throw.
  assert.ok(
    /PROVENANCE_STATES\.includes\(provenance\)/.test(SOURCE),
    'the decoder must check membership explicitly',
  )
  assert.ok(
    !/provenance[^\n]*\?\?\s*'/.test(SOURCE),
    'the decoder must not coalesce an unknown provenance to a fallback',
  )
  assert.ok(
    /may not fall back/.test(SOURCE),
    'the refusal must say why, so the next author does not add the fallback back',
  )
})

test('the decoder refuses an unknown outcome tag rather than guessing', () => {
  assert.ok(
    /refusing rather than guessing/.test(SOURCE),
    'an unrecognised outcome must throw, not render as one of the known three',
  )
})
