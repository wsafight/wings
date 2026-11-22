import { defineCollection, z } from 'astro:content';
import { glob } from 'astro/loaders';

const docsEn = defineCollection({
  loader: glob({ pattern: '*.md', base: '../docs' }),
  schema: z.object({}).passthrough(),
});

const docsZh = defineCollection({
  loader: glob({ pattern: '*.md', base: '../docs/zh' }),
  schema: z.object({}).passthrough(),
});

export const collections = { docsEn, docsZh };
