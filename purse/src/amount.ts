// The amount a person types into Ask for: whole tokens, 1 or more, up to
// the cap a request asks for at a time. Digits of any width count, as
// NFKC reads them (`２０` is 20); leading zeros are dropped.

/** Why a typed amount is not one: nothing, not a whole number, zero, or over the cap. */
export type AmountProblem = 'empty' | 'not_whole' | 'zero' | 'too_big';

/** What {@link parseAmount} makes of a text. */
export type ParsedAmount = { ok: true; amount: number } | { ok: false; problem: AmountProblem };

/** The most tokens one request asks for: as the prototype caps it. */
export const ASK_CAP = 4096;

/** Reads a typed amount. */
export function parseAmount(text: string, cap = ASK_CAP): ParsedAmount {
  const typed = text.normalize('NFKC').trim();
  if (typed === '') {
    return { ok: false, problem: 'empty' };
  }
  if (!/^[0-9]+$/.test(typed)) {
    return { ok: false, problem: 'not_whole' };
  }
  const amount = Number(typed);
  if (amount === 0) {
    return { ok: false, problem: 'zero' };
  }
  if (amount > cap) {
    return { ok: false, problem: 'too_big' };
  }
  return { ok: true, amount };
}
