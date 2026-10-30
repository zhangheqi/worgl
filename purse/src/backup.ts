// Backup (#backup): what the last backup covers of what the purse holds,
// a new one saved as an encrypted file, and a file restored. It covers
// every mint the purse holds, so its top bar shows no mint's face; with no
// mint yet, it is the first run's way to restore. The words and the
// arithmetic are cover.ts's; this builds them into elements.

import { type Child, h, s } from './dom.ts';
import type { MintRecord } from './entries.ts';
import { f2 } from './format.ts';
import {
  type Checking,
  type Cover,
  type HeldNote,
  checkingWords,
  coverLab,
  coverOf,
  coverWords,
  pendingWords,
  savedWhen,
  storageWords,
} from './cover.ts';
import { fileIcon } from './icons.ts';
import { shape, squaresOf } from './layout.ts';
import { square } from './marks.ts';
import { deskbar, tbSub } from './shell.ts';

/** What Backup shows and what it is doing. The passphrases are never kept here: see app.ts `pass`. */
export interface BackupState {
  /** What the purse holds of each mint now, by mint, as Backup last read it. */
  held: Map<string, { notes: HeldNote[]; total: number }>;
  readError: string | undefined;
  saving: boolean;
  saveError: string | undefined;
  /** What the last save holds, in words. */
  saved: string | undefined;
  /**
   * A save that went as a download, until the person says the file is
   * there: the browser may ask first, or be told no, and says nothing back.
   */
  pending:
    | {
        name: string;
        t: number;
        sealed: string;
        /** What it holds of each mint, and the mints it lists, some of which may hold nothing. */
        holds: ReadonlyMap<string, { ids: string[]; values: number[] }>;
        mints: readonly string[];
      }
    | undefined;
  /** The file chosen to restore from: its name, its text (sealed) and when it last changed. */
  file: { name: string; text: string; t: number } | undefined;
  restoring: boolean;
  restoreError: string | undefined;
  /** What the last restore came to, in lines. */
  restored: string[] | undefined;
  /** The checks of restored notes, while they run and once they have. */
  checking: Checking | undefined;
  /** Whether the browser has agreed to keep the purse's data: `undefined` until asked, or where it cannot say. */
  persisted: boolean | undefined;
  /** A save or restore that ended while Backup was not shown: what it came to is kept for the next visit. */
  unseen: boolean;
  /** The error words drawn last time: drawn again, they are not an alert again, so a screen reader says each once. */
  alerted: ReadonlySet<string>;
}

export const blankBackup = (): BackupState => ({
  held: new Map(),
  readError: undefined,
  saving: false,
  saveError: undefined,
  saved: undefined,
  pending: undefined,
  file: undefined,
  restoring: false,
  restoreError: undefined,
  restored: undefined,
  checking: undefined,
  persisted: undefined,
  unseen: false,
  alerted: new Set(),
});

/** What Backup calls when the person acts. */
export interface BackupActions {
  /** A passphrase field changed: `first` and `again` save, `open` restores. */
  passChanged: (which: 'first' | 'again' | 'open', value: string) => void;
  save: () => void;
  /** The person says the downloaded file is there: now it counts as a backup. */
  kept: () => void;
  /** Downloads the sealed file again. */
  again: () => void;
  /** Opens the browser's file chooser. */
  chooseFile: () => void;
  fileChosen: (file: File | undefined) => void;
  restore: () => void;
}

export interface BackupInput {
  state: BackupState;
  records: readonly MintRecord[];
  standalone: boolean;
  now: number;
  actions: BackupActions;
}

/** The pitch and the side of a note's squares in the cover strip (prototype L1862-1866). */
const PITCH = 6;
const SIDE = 4.4;

/** One note of the strip: solid when the backup covers it, dashed when it moved or came since. */
function noteMark(v: number, covered: boolean): SVGSVGElement {
  const { w, h: rows } = shape(v);
  const offset = covered ? 0 : 0.5;
  const side = covered ? SIDE : SIDE - 1;
  let d = '';
  for (let i = 0; i < squaresOf(v); i++) {
    d += square(Math.floor(i / rows) * PITCH + offset, (i % rows) * PITCH + offset, side);
  }
  return s(
    'svg',
    {
      width: f2(w * PITCH),
      height: f2(4 * PITCH),
      viewBox: `0 ${f2(-(4 - rows) * PITCH)} ${f2(w * PITCH)} ${f2(4 * PITCH)}`,
      'aria-hidden': 'true',
      focusable: 'false',
    },
    s('path', { class: covered ? 'cv-in' : 'cv-out', d }),
  );
}

/** The notes held, largest first, each as covered or not. */
const strip = (cover: Cover): HTMLElement =>
  h('div', { class: 'cover', 'aria-hidden': 'true' }, ...cover.notes.map((note) => noteMark(note.v, note.in)));

/** What went wrong, as an alert when it is new, and as a plain line when it is only drawn again. */
const err = (state: BackupState, words: string, cls: string): HTMLElement =>
  h('p', { class: cls, role: state.alerted.has(words) ? undefined : 'alert' }, words);

/** The latest backup any mint's record keeps: the time Backup shows large. */
const lastSaved = (records: readonly MintRecord[]): number | undefined =>
  records.reduce<number | undefined>((last, record) => (record.backup === undefined ? last : Math.max(last ?? 0, record.backup.t)), undefined);

/** Backup. */
export function backupView(input: BackupInput): HTMLElement {
  const { records } = input;
  const any = records.length > 0;
  return h(
    'div',
    { class: 'view v-bk v-sub flow' },
    deskbar(any, any ? 'backup' : 'first'),
    h(
      'main',
      { class: 'vmain' },
      h(
        'section',
        { class: 'band-p' },
        h(
          'div',
          { class: 'wrap' },
          tbSub('Backup', undefined, any),
          h('h1', { class: 'h1 bk-h1', tabindex: -1, 'data-fk': 'h1' }, 'Keep a ', h('em', {}, 'copy'), '.'),
          ...(any
            ? [lastBackup(input), saveForm(input)]
            : [
                h(
                  'p',
                  { class: 'body mt-12' },
                  'Bring back a purse from a backup file: one saved on another device, or before this browser’s data was cleared.',
                ),
                // the phone's top bar has its way back; the desktop's bar has no first run to go to
                h('p', { class: 'desk-only mt-8' }, h('a', { class: 'tbtn', href: '#first', 'data-fk': 'bk-start' }, 'Back to the start')),
              ]),
        ),
      ),
      h('section', { class: 'band-c' }, h('div', { class: 'wrap' }, h('div', { class: 'c-mid bk-low' }, ...restorePart(input), ...storagePart(input)))),
    ),
  );
}

/** What the last backup covers: when it was saved, and each mint's notes as covered or not. */
function lastBackup(input: BackupInput): HTMLElement {
  const { state, records, now } = input;
  const several = records.length > 1;
  const t = lastSaved(records);
  const covers = records.map((record) => {
    const held = state.held.get(record.mint);
    return { record, cover: held === undefined ? undefined : coverOf(held.notes, record.backup?.ids, held.total) };
  });
  const only = covers.length === 1 ? covers[0] : undefined;
  const lab = only === undefined ? undefined : only.record.backup === undefined ? 'None' : only.cover === undefined ? undefined : coverLab(only.cover);
  const rows: Child[] = covers.map(({ record, cover }) => {
    if (cover === undefined) {
      return several ? h('p', { class: 'body mt-12' }, `${record.name}: `, state.readError === undefined ? 'reading…' : 'not read.') : null;
    }
    return h(
      'div',
      { class: 'bk-mint' },
      several ? h('p', { class: 'lab muted mt-12' }, record.name) : null,
      cover.notes.length === 0 ? null : strip(cover),
      h(
        'p',
        { class: 'body mt-12' },
        coverWords(record.backup === undefined ? undefined : cover, record.backup?.t, now, several ? record.name : undefined),
      ),
    );
  });
  const reading = !several && only !== undefined && only.cover === undefined;
  return h(
    'div',
    { class: 'sec' },
    h('div', { class: 'sec-h' }, h('span', { class: 'lab' }, 'Last backup'), lab === undefined ? null : h('span', { class: 'lab' }, lab)),
    t === undefined ? null : h('p', { class: 'big-when' }, savedWhen(t, now)),
    ...rows,
    reading && state.readError === undefined ? h('p', { class: 'body mt-12', role: 'status' }, 'Reading what the purse holds…') : null,
    // a read that failed is said even where an older read still draws: what it shows may be out of date
    state.readError === undefined ? null : err(state, state.readError, 'err mt-12'),
    t === undefined
      ? null
      : h(
          'p',
          { class: 'cv-key lab', 'aria-hidden': 'true' },
          h('span', {}, h('i', { class: 'k-in' }), 'Covered'),
          h('span', {}, h('i', { class: 'k-out' }), 'Moved or new since'),
        ),
  );
}

/** A passphrase field: never given its value as an attribute, so it is never in the page's markup (app.ts sets it). */
function passField(id: string, label: string, which: 'first' | 'again' | 'open', autocomplete: string, input: BackupInput): HTMLElement {
  return h(
    'div',
    { class: 'fld' },
    h('label', { class: 'lab', for: id }, label),
    h('input', {
      class: 'inp',
      id,
      type: 'password',
      'data-fk': id,
      autocomplete,
      on: { input: (event) => input.actions.passChanged(which, (event.target as HTMLInputElement).value) },
    }),
  );
}

function saveForm(input: BackupInput): HTMLElement {
  const { state, actions } = input;
  return h(
    'form',
    {
      class: 'sec form',
      'data-form': 'backup',
      novalidate: true,
      on: {
        submit: (event) => {
          event.preventDefault();
          actions.save();
        },
      },
    },
    h('div', { class: 'sec-h' }, h('span', { class: 'lab' }, 'Save a new backup')),
    h('p', { class: 'body' }, 'The purse saves an encrypted file of the notes it holds now. You need the file and the passphrase to bring them back.'),
    passField('bk-p1', 'Passphrase', 'first', 'new-password', input),
    passField('bk-p2', 'Type it again', 'again', 'new-password', input),
    state.saveError === undefined ? null : err(state, state.saveError, 'err'),
    h(
      'button',
      { class: 'btn solid wide', type: 'submit', disabled: state.saving || state.pending !== undefined, 'data-fk': 'bk-save' },
      state.saving ? 'Sealing…' : 'Save backup',
    ),
    state.pending === undefined
      ? null
      : h(
          'div',
          { class: 'bk-pending' },
          h('p', { class: 'result', role: 'status' }, pendingWords(state.pending.name)),
          h(
            'div',
            { class: 'btn-row' },
            h('button', { class: 'btn solid', type: 'button', 'data-fk': 'bk-kept', on: { click: actions.kept } }, 'It is saved'),
            h('button', { class: 'btn', type: 'button', 'data-fk': 'bk-again', on: { click: actions.again } }, 'Download again'),
          ),
        ),
    state.saved === undefined ? null : h('p', { class: 'result', role: 'status' }, state.saved),
  );
}

function restorePart(input: BackupInput): Child[] {
  const { state, actions } = input;
  const chooser = h('input', {
    type: 'file',
    id: 'bk-file',
    hidden: true,
    accept: '.worgl,application/octet-stream,application/json',
    on: {
      change: (event) => {
        const files = (event.target as HTMLInputElement).files;
        actions.fileChosen(files?.[0] ?? undefined);
      },
    },
  });
  const file = state.file;
  const open =
    file === undefined
      ? [
          h(
            'button',
            { class: 'btn wide mt-14', type: 'button', 'data-fk': 'bk-restore', on: { click: actions.chooseFile } },
            fileIcon(),
            'Restore from a backup file',
          ),
        ]
      : [
          h(
            'form',
            {
              class: 'form',
              'data-form': 'restore',
              novalidate: true,
              on: {
                submit: (event) => {
                  event.preventDefault();
                  actions.restore();
                },
              },
            },
            h('p', { class: 'note-line bk-file' }, fileIcon(), h('span', {}, file.name)),
            passField('bk-open', 'Its passphrase', 'open', 'current-password', input),
            h(
              'div',
              { class: 'btn-row' },
              h('button', { class: 'btn solid', type: 'submit', disabled: state.restoring, 'data-fk': 'bk-go' }, state.restoring ? 'Opening…' : 'Restore'),
              h('button', { class: 'btn', type: 'button', disabled: state.restoring, 'data-fk': 'bk-other', on: { click: actions.chooseFile } }, 'Another file'),
            ),
          ),
        ];
  return [
    h('p', { class: 'lab muted' }, 'Restore'),
    h(
      'p',
      { class: 'body mt-8' },
      'Restoring merges: tokens this purse holds already are skipped. The rest are checked with the mint, a few seconds apart, and those spent since the backup are dropped.',
    ),
    chooser,
    ...open,
    state.restoreError === undefined ? null : err(state, state.restoreError, 'err flow-err'),
    state.restored === undefined || state.restored.length === 0
      ? null
      : h('div', { class: 'result mt-14', role: 'status' }, ...state.restored.map((line) => h('p', {}, line))),
    state.checking === undefined ? null : h('p', { class: 'note-line mt-10', role: 'status' }, checkingWords(state.checking)),
  ];
}

function storagePart(input: BackupInput): Child[] {
  return [
    h('p', { class: 'lab muted mt-26' }, 'Storage'),
    ...storageWords(input.state.persisted, input.standalone).map((line, i) => h('p', { class: i === 0 ? 'body mt-8' : 'note-line mt-8' }, line)),
  ];
}
