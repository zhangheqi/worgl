# The Worgl website

The site for worgl.dev, built with [Astro](https://astro.build). It makes no
third-party requests: the fonts are served from the site, and there are no
analytics or cookies.

```sh
npm ci
npm run dev     # http://localhost:4321
npm run check   # type-checks the pages and scripts
npm run build   # writes the site to dist/
```

- `src/pages/`: one file per page.
- `src/components/figures/`: the live figures, each an Astro component with
  its script in `src/scripts/` and its styles in `src/styles/figures/`. They
  are plain TypeScript with no dependencies, and they pause off screen, in
  background tabs and for visitors who prefer reduced motion.
- `src/styles/tokens.css`: the colour, type and measure tokens for both
  themes, shared by the designed pages and the docs.
- `src/styles/global.css`: type and layout for the designed pages.
- `src/content/docs/docs/`: the docs, one Markdown file per page, served
  under `/docs/` by [Starlight](https://starlight.astro.build). The sidebar
  is in `astro.config.mjs`, the theme in `src/styles/docs.css`, and the
  header and footer overrides in `src/components/docs/`. Code samples that
  CI compiles, such as the crates' doc examples, are imported from the
  crates rather than copied, and the changelog page shows `CHANGELOG.md`.

Every figure the site quotes links to the README section that measured it,
and nothing that does not exist yet is described as if it did.

`.github/workflows/site.yml` builds the site on every pull request that
touches it. On a merge to `main` it also deploys to GitHub Pages, once Pages
is set to deploy from GitHub Actions in the repository's settings. The
workflow passes the address Pages serves from as `SITE_URL` and `SITE_BASE`,
so the same build works under the repository's path or at worgl.dev. The
deploy also runs `cargo doc` and serves the result at `api/rust/`, which the
docs' sidebar links to, so that link works only on the deployed site.
