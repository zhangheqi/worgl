// @ts-check
import { defineConfig } from 'astro/config';

// GitHub Pages serves the site under the repository's path until worgl.dev
// points at it. The deploy workflow passes the address Pages reports, so the
// same build works at either; locally the site is served from the root.
export default defineConfig({
  site: process.env.SITE_URL || 'http://localhost:4321',
  base: process.env.SITE_BASE || '/',
  trailingSlash: 'ignore',
  build: { format: 'directory' },
});
