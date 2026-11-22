import { visit } from 'unist-util-visit';

const REPO_BLOB = 'https://github.com/wsafight/wings/blob/main';

function joinBase(base, subpath) {
  const cleanBase = base.endsWith('/') ? base.slice(0, -1) : base;
  const cleanSub = subpath.startsWith('/') ? subpath : `/${subpath}`;
  return `${cleanBase}${cleanSub}`;
}

function extractText(node) {
  if (!node) return '';
  if (typeof node.value === 'string') return node.value;
  if (Array.isArray(node.children)) {
    return node.children.map(extractText).join('');
  }
  return '';
}

export function remarkWingsDocs(options = {}) {
  const base = options.base || '/';

  return (tree, file) => {
    const filePath = String(file?.path || file?.history?.[0] || '').replace(/\\/g, '/');
    const isZh = filePath.includes('/docs/zh/');

    if (Array.isArray(tree.children) && tree.children.length > 0) {
      if (tree.children[0]?.type === 'heading' && tree.children[0]?.depth === 1) {
        tree.children.shift();
      }
      if (tree.children[0]?.type === 'paragraph') {
        const firstPara = extractText(tree.children[0]).trim();
        if (
          firstPara.startsWith('English') ||
          firstPara.startsWith('返回概览') ||
          firstPara.startsWith('Back to Overview')
        ) {
          tree.children.shift();
        }
      }
    }

    visit(tree, 'link', (node) => {
      if (typeof node.url !== 'string') return;
      const url = node.url;
      if (/^(https?:|mailto:|#)/.test(url)) return;

      if (isZh) {
        const zhReadmeMatch = url.match(/^\.\.\/\.\.\/README\.zh-CN\.md(#.*)?$/);
        if (zhReadmeMatch) {
          node.url = `${joinBase(base, '/zh/')}${zhReadmeMatch[1] || ''}`;
          return;
        }

        const enReadmeMatch = url.match(/^\.\.\/\.\.\/README\.md(#.*)?$/);
        if (enReadmeMatch) {
          node.url = `${joinBase(base, '/')}${enReadmeMatch[1] || ''}`;
          return;
        }

        const parentEnDocMatch = url.match(/^\.\.\/([a-z0-9-]+)\.md(#.*)?$/);
        if (parentEnDocMatch) {
          node.url = `${joinBase(base, `/docs/${parentEnDocMatch[1]}/`)}${parentEnDocMatch[2] || ''}`;
          return;
        }

        const siblingZhDocMatch = url.match(/^(?:\.\/)?([a-z0-9-]+)\.md(#.*)?$/);
        if (siblingZhDocMatch) {
          node.url = `${joinBase(base, `/zh/docs/${siblingZhDocMatch[1]}/`)}${siblingZhDocMatch[2] || ''}`;
          return;
        }

        const repoRootMatch = url.match(/^\.\.\/\.\.\/(.+)$/);
        if (repoRootMatch) {
          node.url = `${REPO_BLOB}/${repoRootMatch[1]}`;
          return;
        }
      } else {
        const enReadmeMatch = url.match(/^\.\.\/README\.md(#.*)?$/);
        if (enReadmeMatch) {
          node.url = `${joinBase(base, '/')}${enReadmeMatch[1] || ''}`;
          return;
        }

        const zhReadmeMatch = url.match(/^\.\.\/README\.zh-CN\.md(#.*)?$/);
        if (zhReadmeMatch) {
          node.url = `${joinBase(base, '/zh/')}${zhReadmeMatch[1] || ''}`;
          return;
        }

        const childZhDocMatch = url.match(/^(?:\.\/)?zh\/([a-z0-9-]+)\.md(#.*)?$/);
        if (childZhDocMatch) {
          node.url = `${joinBase(base, `/zh/docs/${childZhDocMatch[1]}/`)}${childZhDocMatch[2] || ''}`;
          return;
        }

        const siblingEnDocMatch = url.match(/^(?:\.\/)?([a-z0-9-]+)\.md(#.*)?$/);
        if (siblingEnDocMatch) {
          node.url = `${joinBase(base, `/docs/${siblingEnDocMatch[1]}/`)}${siblingEnDocMatch[2] || ''}`;
          return;
        }

        const repoRootMatch = url.match(/^\.\.\/(.+)$/);
        if (repoRootMatch) {
          node.url = `${REPO_BLOB}/${repoRootMatch[1]}`;
        }
      }
    });
  };
}
