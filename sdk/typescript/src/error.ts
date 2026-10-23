/** No fields. */
type NoFields = Record<string, never>;

/**
 * The fields each {@link WorglError} code carries in its `details`.
 * Amounts and generations are `bigint`, counts and limits `number`, and
 * keys hex strings.
 */
export interface WorglErrorDetails {
  /**
   * An argument is not the JSON the call takes: not a payment request, a
   * payment, notes, a mint's info or keys, or a saved purse; or a transfer
   * spends no notes.
   */
  malformed: NoFields;
  /** The mint's info lists no layers. */
  no_layers: NoFields;
  /** The saved purse is in `format`, a newer one this SDK does not read. */
  unknown_format: { format: number };
  /** A request for nothing: the amount is zero. */
  zero_amount: NoFields;
  /** A request for `amount`, which cannot be split into the mint's denominations. */
  unsplittable: { amount: bigint };
  /** A request that would take more than `max` notes. */
  too_many_notes: { max: number };
  /**
   * The request is for another mint than the info given, or the payment
   * spends notes of another mint than the request's.
   */
  wrong_mint: NoFields;
  /** The request asks for nothing. */
  empty_request: NoFields;
  /** The request asks for a note of `amount`, which is not one of the mint's denominations. */
  denomination: { amount: bigint };
  /**
   * The request asks for a note to `holder`, a key the purse has paid
   * already, such as one request twice. Paying again takes a new request.
   */
  paid: { holder: string };
  /**
   * Too little can be spent: `spendable` now, and the request asks for
   * `needed`. Frozen notes become spendable at the next rotation.
   */
  unaffordable: { spendable: bigint; needed: bigint };
  /**
   * Enough can be spent, but in notes too small for the notes the request
   * asks for: the largest is `largest`. Ask the payee for a request in
   * notes of at most that, as `requestAtMost` makes.
   */
  fragmented: { largest: bigint };
  /** The change would take more than the `maxOutputs` notes one transfer may make. */
  too_many_outputs: { maxOutputs: number };
  /** A transfer planned for the payment breaks the mint's policy: a bug. The message says how. */
  policy: NoFields;
  /** Signing the payment failed, which means the saved purse is corrupt. */
  sign: NoFields;
  /** A transfer of the payment spends more or fewer than one note. */
  not_one_input: NoFields;
  /** The payment does not pay the note of `amount` to `holder` that the request asks for. */
  unpaid: { amount: bigint; holder: string };
  /**
   * The payment pays `amount` to `holder`, a key the request asks a note
   * for, more than once or with another amount.
   */
  overpaid: { amount: bigint; holder: string };
  /** A note is of another mint than the keys it is checked against. */
  wrong_issuer: NoFields;
  /**
   * The mint's keys hold none for a note's `generation`: its layer was
   * dropped, or the keys predate it.
   */
  unknown_generation: { generation: bigint };
  /** A note's signature does not verify: it is forged or altered. */
  bad_signature: NoFields;
  /** The faucet hands out notes of `amount`, which its mint cannot make as one note. */
  faucet_amount: { amount: bigint };
}

/** The stable code of a {@link WorglError}. */
export type WorglErrorCode = keyof WorglErrorDetails;

/**
 * What the SDK refuses itself, in its Rust core or around it, such as a
 * payment the purse cannot make: an `Error` named `WorglError`, with a
 * stable `code` and that code's fields in `details`. `message` is for
 * people and may change. {@link isWorglError} tells one apart, and checking
 * `code` then gives `details` their type:
 *
 * ```ts
 * try {
 *   await alice.pay(request);
 * } catch (error) {
 *   if (!(isWorglError(error) && error.code === 'fragmented')) {
 *     throw error;
 *   }
 *   // Alice's notes are too small for the ones Bob asked for: he asks again.
 *   await alice.pay(await bob.requestAtMost(mint, request.amount, error.details.largest));
 * }
 * ```
 *
 * A refusal from the mint is a `MintError` instead.
 */
export type WorglError = {
  [C in WorglErrorCode]: Error & {
    readonly name: 'WorglError';
    readonly code: C;
    readonly details: Readonly<WorglErrorDetails[C]>;
  };
}[WorglErrorCode];

/** Whether `error` is a {@link WorglError}. */
export function isWorglError(error: unknown): error is WorglError {
  return (
    error instanceof Error && error.name === 'WorglError' && 'code' in error && 'details' in error
  );
}

/** A {@link WorglError} raised outside the Rust core, made as the core makes its own. */
export function worglError<C extends WorglErrorCode>(
  code: C,
  details: WorglErrorDetails[C],
  message: string,
): WorglError {
  return Object.assign(new Error(message), { name: 'WorglError', code, details }) as WorglError;
}
