import { defineConfig } from 'astro/config';
import { remarkWingsDocs } from './src/plugins/remark-wings-docs.mjs';

const rawBase = process.env.ASTRO_BASE || '/';
const normalizedBase = rawBase === '/' ? '/' : `/${rawBase.replace(/^\/+|\/+$/g, '')}/`;

export default defineConfig({
  site: process.env.ASTRO_SITE || 'https://wsafight.github.io',
  base: normalizedBase,
  trailingSlash: 'always',
  i18n: {
    defaultLocale: 'en',
    locales: ['en', 'zh'],
    routing: {
      prefixDefaultLocale: false,
    },
  },
  markdown: {
    remarkPlugins: [[remarkWingsDocs, { base: normalizedBase }]],
    shikiConfig: {
      theme: 'github-dark-default',
      langAlias: {
        wings: 'rust',
      },
    },
  },
});
