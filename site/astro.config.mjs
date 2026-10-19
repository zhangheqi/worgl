// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// SITE_URL and SITE_BASE come from actions/configure-pages in CI, so the
// site works at a custom domain or at /<repo>/ on github.io alike.
export default defineConfig({
  site: process.env.SITE_URL || 'http://localhost:4321',
  base: process.env.SITE_BASE || '/',
  trailingSlash: 'ignore',
  build: { format: 'directory' },
  integrations: [
    starlight({
      title: 'Worgl',
      description:
        'Worgl is an open-source issuer for tokens that cost one bit to track, need no accounts, and expire if they sit still.',
      favicon: '/favicon.svg',
      // The site's own 404 page stays.
      disable404Route: true,
      credits: false,
      lastUpdated: false,
      editLink: { baseUrl: 'https://github.com/zhangheqi/worgl/edit/main/site/' },
      customCss: [
        '@fontsource/instrument-serif/400.css',
        '@fontsource/instrument-serif/400-italic.css',
        '@fontsource-variable/instrument-sans/wght.css',
        '@fontsource-variable/martian-mono/wdth.css',
        './src/styles/docs.css',
      ],
      components: {
        SiteTitle: './src/components/docs/SiteTitle.astro',
        SocialIcons: './src/components/docs/SiteLinks.astro',
        Footer: './src/components/docs/Footer.astro',
      },
      // Code on the site's code tint, square, with no window chrome unless a block has a title.
      expressiveCode: {
        themes: ['github-dark-default', 'github-light-default'],
        defaultProps: { frame: 'code' },
        styleOverrides: {
          borderRadius: '0',
          borderColor: 'var(--rule)',
          codeBackground: 'var(--code-bg)',
          codeFontFamily: 'var(--sl-font-mono)',
          uiFontFamily: 'var(--sl-font)',
          codeFontSize: '0.8125rem',
          codeLineHeight: '1.75',
          frames: {
            shadowColor: 'transparent',
            editorBackground: 'var(--code-bg)',
            editorTabBarBackground: 'var(--paper-2)',
            editorTabBarBorderBottomColor: 'var(--rule)',
            editorActiveTabBackground: 'var(--code-bg)',
            editorActiveTabForeground: 'var(--ink)',
            editorActiveTabIndicatorTopColor: 'var(--pink)',
            editorActiveTabIndicatorBottomColor: 'transparent',
            terminalBackground: 'var(--code-bg)',
            terminalTitlebarBackground: 'var(--paper-2)',
            terminalTitlebarBorderBottomColor: 'var(--rule)',
            terminalTitlebarForeground: 'var(--muted)',
            terminalTitlebarDotsForeground: 'var(--rule-strong)',
          },
        },
      },
      sidebar: [
        {
          label: 'Start',
          items: [
            { label: 'Quickstart', slug: 'docs/quickstart' },
            { label: 'Concepts', slug: 'docs/concepts' },
            { label: 'Choosing a period and layer count', slug: 'docs/choosing-a-period' },
          ],
        },
        {
          label: 'Build',
          items: [
            { label: 'Rust SDK', slug: 'docs/rust-sdk' },
            { label: 'TypeScript SDK', slug: 'docs/typescript-sdk' },
            { label: 'The worgl command', slug: 'docs/command-line' },
            { label: 'Embedding the library', slug: 'docs/embedding' },
          ],
        },
        {
          label: 'Operate',
          items: [
            { label: 'Running a server', slug: 'docs/running-a-server' },
            { label: 'Running a cluster', slug: 'docs/running-a-cluster' },
            { label: 'Running a faucet', slug: 'docs/running-a-faucet' },
            { label: 'Mint authorities and keys', slug: 'docs/keys' },
            { label: 'Pausing a mint', slug: 'docs/pausing' },
            { label: 'Backups', slug: 'docs/backups' },
            { label: 'Exporting a log for audit', slug: 'docs/audit' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'HTTP API', slug: 'docs/http-api' },
            { label: 'Error codes', slug: 'docs/error-codes' },
            { label: 'Note format', slug: 'docs/note-format' },
            { label: 'Policy fields', slug: 'docs/policy' },
            // Built by cargo doc in the deploy job; it exists only on the deployed site.
            { label: 'Rust API (rustdoc)', link: '/api/rust/worgl/' },
            { label: 'Changelog', slug: 'docs/changelog' },
          ],
        },
        { label: 'Privacy model', slug: 'docs/privacy' },
      ],
    }),
  ],
  vite: {
    // The docs import samples that CI compiles, from outside site/.
    server: { fs: { allow: ['..'] } },
  },
});
