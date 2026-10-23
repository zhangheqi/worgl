/** No fields. */
type NoFields = Record<string, never>;

/**
 * The fields each {@link WorglError} code carries in its `details`.
 * Amounts and generations are `bigint`, counts and limits `number`, keys
 * hex strings, and a field with no value `undefined`.
 */
export interface WorglErrorDetails {
  /**
   * An argument is not the JSON the call takes: not a payment request, a
   * payment, notes, a mint's info or keys, or a saved purse, which a purse
   * reads when it opens and, on a store with a lock, in each call that takes
   * the lock; or a transfer spends no notes.
   */
  malformed: NoFields;
  /** The mint's info lists no layers. */
  no_layers: NoFields;
  /**
   * The saved purse, or the purse a backup holds, is in `format`, a newer
   * one this SDK does not read. A purse reads the saved one when it opens
   * and, on a store with a lock, in each call that takes the lock, so a page
   * left open meets it once another tab saves with a newer SDK.
   */
  unknown_format: { format: number };
  /** The text is not a backup or purse file. */
  not_sealed: NoFields;
  /** The backup is of `version`, a newer one this SDK does not read. */
  unknown_version: { version: number };
  /**
   * The passphrase is wrong, or the backup was altered after it was sealed:
   * the two look the same.
   */
  wrong_passphrase: NoFields;
  /**
   * A part of the backup does not read, such as its salt, settings or
   * ciphertext, or it opened to no purse. The message says which.
   */
  damaged: NoFields;
  /**
   * Too little memory to derive the backup's key: 64 MiB, or up to 256 MiB
   * for a backup whose settings ask for more.
   */
  out_of_memory: NoFields;
  /** The system's random number generator failed, so no backup can be sealed. */
  no_randomness: NoFields;
  /**
   * The passphrase for a new backup is empty, which would let anyone who
   * gets the file spend its notes. A backup opens with whatever passphrase
   * sealed it.
   */
  empty_passphrase: NoFields;
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
   * asks for: the largest is `largest`. A request for the same amount in
   * notes of at most `atMost`, as `requestAtMost` makes, can be paid now,
   * and none in larger notes can. `atMost` is `undefined` when no such
   * request can be paid, as when it would take too many notes: ask for
   * less, or wait for frozen notes to become spendable.
   */
  fragmented: { largest: bigint; atMost: bigint | undefined };
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
  /**
   * An answer to a transfer holds `answered` notes for the transfer's
   * `outputs` outputs, so it is not the mint's.
   */
  miscounted: { outputs: number; answered: number };
  /**
   * Note `index` of an answer to a transfer is not the transfer's output in
   * its place: it is another mint's, or of another amount or key.
   */
  unasked: { index: number };
  /**
   * Note `index` of an answer to a transfer does not carry the mint's
   * signature: it is forged or altered, or the keys do not know its
   * generation.
   */
  unsigned: { index: number };
  /** The faucet hands out notes of `amount`, which its mint cannot make as one note. */
  faucet_amount: { amount: bigint };
  /**
   * A browser store could not open its database: another tab holds it open
   * in an older version and did not close it when asked to.
   */
  store_blocked: NoFields;
  /**
   * A browser store saved nothing: the change lost the store's lock while it
   * was made, as when another tab took the lock with `steal`, or another
   * writer saved since the change read the store. The purse undid the
   * change, and reads the store again before the next.
   */
  store_changed: NoFields;
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
 *   if (
 *     !(isWorglError(error) && error.code === 'fragmented') ||
 *     error.details.atMost === undefined
 *   ) {
 *     throw error;
 *   }
 *   // Alice's notes are too small for the ones Bob asked for: he drops
 *   // that request and asks again, in notes she can pay.
 *   await bob.withdraw(request);
 *   await alice.pay(await bob.requestAtMost(mint, request.amount, error.details.atMost));
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
