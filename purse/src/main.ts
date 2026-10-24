// The purse's entry: the fonts, self-hosted, the styles, then the app.
// Nothing here imports the SDK, so the shell paints before its wasm loads.

import '@fontsource/instrument-serif/400.css';
import '@fontsource/instrument-serif/400-italic.css';
import '@fontsource-variable/instrument-sans/wght.css';
import '@fontsource-variable/martian-mono/wdth.css';
import './styles.css';

import { start } from './app.ts';

void start();
