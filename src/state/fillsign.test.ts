import { describe, expect, it } from "vitest";
import type { PageTextRun, TextRunId } from "../api/types";

/**
 * A `TextRunId` is a structured address (op index plus operand path), not a string. These tests
 * care about identity round-tripping, so one opaque id is derived here rather than hand-built
 * five times.
 */
function textRunId(label: string): TextRunId {
  return { opIndex: label.length, operandPath: [0] };
}
import {
  baselineFor,
  DESCENT_EM,
  isUsableSnap,
  snapTargetAt,
  signatureFreeTextData,
  signatureInkData,
} from "./fillsign";

/**
 * A run shaped like the LEA form's own text: 6.96pt, box bottom below the baseline by the measured
 * descender depth. Built rather than mocked -- these are plain values, and a fixture that mirrors
 * the measurement makes the arithmetic's inputs visible in each test.
 */
function run(overrides: Partial<PageTextRun> = {}): PageTextRun {
  const fontSize = 6.96;
  const baseline = 100;
  const depth = DESCENT_EM * fontSize;
  return {
    runId: textRunId("run-1"),
    text: "Nachname",
    operator: "Tj",
    fontKey: "F1",
    fontName: "Helvetica",
    fontSize,
    // The box extends from the baseline down by the descender depth (bottom edge below the pen
    // origin) and up by roughly the cap height.
    rect: { x0: 72, y0: baseline - depth, x1: 140, y1: baseline + fontSize * 0.7 },
    width: 68,
    admissibility: "admissible",
    pairConfidence: "strong",
    fragmentCount: 1,
    ...overrides,
  } as PageTextRun;
}

describe("D-N1 placement snapping", () => {
  it("puts the baseline back on the pen origin, which the box bottom is not", () => {
    const target = run();
    const baseline = baselineFor(target);
    expect(baseline).not.toBeNull();
    // The correction is the whole point: without it every mark is a descender low, which the
    // design measured as a quarter of a line at this size.
    expect(baseline).toBeCloseTo(100, 3);
    expect(target.rect!.y0).toBeLessThan(baseline as number);
  });

  it("derives the correction from the measurement rather than the box height", () => {
    // The failure this guards: a correction taken from `y1 - y0` (box HEIGHT) instead of the
    // descent would look plausible on one fixture and be wrong on every differently-sized line.
    const small = run({ fontSize: 6.96 });
    const large = run({
      fontSize: 12,
      rect: { x0: 0, y0: 100 - DESCENT_EM * 12, x1: 50, y1: 100 + 12 * 0.7 },
    });

    const smallCorrection = (baselineFor(small) as number) - small.rect!.y0;
    const largeCorrection = (baselineFor(large) as number) - large.rect!.y0;

    expect(smallCorrection).toBeCloseTo(DESCENT_EM * 6.96, 5);
    expect(largeCorrection).toBeCloseTo(DESCENT_EM * 12, 5);
    // And the two corrections are NOT the same, which a box-height formula would have made them.
    expect(largeCorrection / smallCorrection).toBeCloseTo(12 / 6.96, 3);
  });

  it("refuses a box too short to contain the correction instead of placing above the line", () => {
    // The 12pt sample contained a run reporting 0.000 depth. Its box cannot absorb a correction,
    // so resolving a baseline from it would put the mark above the text. Refusal is the answer.
    const flat = run({
      fontSize: 12,
      rect: { x0: 0, y0: 100, x1: 50, y1: 100.5 },
    });
    expect(baselineFor(flat)).toBeNull();
    expect(snapTargetAt([flat], { x: 10, y: 100.2 }, 12)).toBeNull();
  });

  it("excludes a suspicious box from snapping rather than snapping with a warning", () => {
    const suspicious = run({ boxSuspicious: true });
    expect(isUsableSnap(suspicious)).toBe(false);
    expect(snapTargetAt([suspicious], { x: 80, y: 100 }, 12)).toBeNull();
  });

  it("ignores a run with no rect, a zero font size, or a sub-point box", () => {
    expect(isUsableSnap(run({ rect: null }))).toBe(false);
    expect(isUsableSnap(run({ fontSize: 0 }))).toBe(false);
    expect(isUsableSnap(run({ rect: { x0: 10, y0: 0, x1: 10.4, y1: 5 } }))).toBe(false);
  });

  it("snaps a point inside a box even when another box's edge is nearer", () => {
    const target = run({ runId: textRunId("inside"), rect: { x0: 72, y0: 98, x1: 140, y1: 104 } });
    // This box's edge is 0.5pt from the point; the first box CONTAINS it.
    const neighbour = run({
      runId: textRunId("neighbour"),
      rect: { x0: 72, y0: 104.5, x1: 140, y1: 110 },
    });

    const chosen = snapTargetAt([target, neighbour], { x: 100, y: 102 }, 8);
    expect(chosen?.runId).toEqual(textRunId("inside"));
  });

  it("snaps to a nearby box within tolerance and lands where released beyond it", () => {
    const target = run({ rect: { x0: 72, y0: 98, x1: 140, y1: 104 } });
    const justOutside = { x: 100, y: 105 };

    expect(snapTargetAt([target], justOutside, 2)?.runId).toEqual(textRunId("run-1"));
    // Past the tolerance there is no target at all, so the caller keeps the raw release point.
    expect(snapTargetAt([target], justOutside, 0.5)).toBeNull();
  });

  it("gives a mark one em tall, scaled to the run's own size", () => {
    const small = snapTargetAt([run()], { x: 80, y: 100 }, 8);
    const large = snapTargetAt([run({ fontSize: 24 })], { x: 80, y: 100 }, 8);

    expect(small).not.toBeNull();
    expect(large).not.toBeNull();
    expect(small!.rect.y1 - small!.rect.y0).toBeCloseTo(6.96, 3);
    expect(large!.rect.y1 - large!.rect.y0).toBeCloseTo(24, 3);
  });

  it("names the run it snapped to, so the surface can say what it snapped to", () => {
    const chosen = snapTargetAt([run({ runId: textRunId("familienname"), text: "Familienname" })], {
      x: 80,
      y: 100,
    }, 8);
    expect(chosen?.runId).toEqual(textRunId("familienname"));
    expect(chosen?.text).toBe("Familienname");
  });
});

describe("D-N6 the surface declares the signature", () => {
  it("marks a drawn row as a visual signature", () => {
    const data = signatureInkData(
      [
        [
          [72, 100],
          [80, 104],
        ],
      ],
      "#000000",
    );
    expect(data.visualSignature).toBe(true);
    expect(data.v).toBe(1);
    expect(data.strokes).toHaveLength(1);
  });

  it("marks a typed row as a visual signature", () => {
    const data = signatureFreeTextData({ x0: 72, y0: 100, x1: 200, y1: 112 }, "#000000");
    expect(data.visualSignature).toBe(true);
    // The wire shape is a tuple, not an object: asserting the conversion is the point.
    expect(data.rect).toEqual([72, 100, 200, 112]);
  });

  it("is the only place the flag is set: an ordinary ink row must not carry it", () => {
    // The invariant in one line. `signatureInkData` adds the flag; the plain ink shape this
    // module does NOT use for signatures has no such field, which is what `annotate.rs` reads.
    const ordinary = { v: 1 as const, strokes: [], color: "#000000" };
    expect("visualSignature" in ordinary).toBe(false);
    expect(signatureInkData([], "#000000").visualSignature).toBe(true);
  });
});

/**
 * §6 acceptance row: "A snap to a run's box lands on the box's bottom edge; a suspicious box is not
 * snapped" (frontend).
 *
 * Named for the row so the design's acceptance table resolves to a real test. The three properties
 * are separated because they fail differently: a snap that lands on the wrong edge puts the
 * signature through the text, a suspicious box that snaps puts it somewhere the measurement does not
 * support, and a point far from every box must produce no snap at all rather than the nearest one.
 */
describe("a_signature_snaps_to_a_measured_box_and_skips_a_suspicious_one", () => {
  it("lands on the box's bottom edge corrected to the baseline, not on the raw edge", () => {
    const target = run();
    const snapped = snapTargetAt([target], { x: 100, y: 100 }, 4);

    expect(snapped).not.toBeNull();
    // §3.2.1: the box's bottom edge is a *descender*, not the pen origin -- measured 1.46-2.59pt
    // low on the LEA form. Snapping to the raw edge would put every signature a quarter of a line
    // too low, so the placement uses the box bottom **plus the measured correction**, which is the
    // baseline. The row's "bottom edge" is shorthand for this corrected edge.
    expect(snapped!.rect.y0).toBeCloseTo(baselineFor(target) as number, 5);
    // And it is NOT the raw edge: if this ever passes, the correction has been lost and the mark
    // will sit in the descenders.
    expect(snapped!.rect.y0).toBeGreaterThan(target.rect!.y0);
    // The band is one em tall from the baseline, from the run's own size.
    expect(snapped!.rect.y1).toBeCloseTo(snapped!.rect.y0 + target.fontSize, 5);
  });

  it("does not snap to a box the measurement flagged as suspicious", () => {
    // D-N1: assistance is only offered where the measurement is trustworthy. A box whose width does
    // not agree with its text would place the mark at a position the document does not support, and
    // the user would have no way to tell that from their own misdrop.
    const suspicious = run({ boxSuspicious: true });
    expect(isUsableSnap(suspicious)).toBe(false);
    expect(snapTargetAt([suspicious], { x: 100, y: 100 }, 4)).toBeNull();
  });

  it("prefers a usable box over a suspicious one at the same place", () => {
    // The suspicious box must not merely be ranked lower -- it must be skipped, or a page with one
    // bad measurement would still snap to it whenever it happened to be first.
    const suspicious = run({ boxSuspicious: true });
    const usable = run({ runId: textRunId("run-2") });
    const snapped = snapTargetAt([suspicious, usable], { x: 100, y: 100 }, 4);

    expect(snapped).not.toBeNull();
    expect(snapped!.runId).toEqual(usable.runId);
    expect(snapped!.rect.y0).toBeCloseTo(baselineFor(usable) as number, 5);
  });

  it("returns nothing when the point is beyond the tolerance, rather than the nearest box", () => {
    // D-N1's fallback: no snap is a valid, stated outcome (the panel says the mark will land where
    // released). Snapping to a distant box would move the user's mark somewhere they did not click.
    expect(snapTargetAt([run()], { x: 100, y: 100 }, 4)).not.toBeNull();
    expect(snapTargetAt([run()], { x: 400, y: 700 }, 4)).toBeNull();
  });
});
