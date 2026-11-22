export type Locale = 'en' | 'zh';

export interface DocMeta {
  slug: string;
  order: number;
  group: string;
  title: string;
  shortTitle: string;
  summary: string;
  sourceFile: string;
  keywords: string[];
}

export const DOC_ITEMS_BY_LOCALE: Record<Locale, DocMeta[]> = {
  en: [
    {
      slug: 'product',
      order: 1,
      group: 'Positioning & Design',
      title: 'Product Positioning',
      shortTitle: 'Product Positioning',
      summary:
        'Performance and memory density goals, baseline trade-offs vs. BEAM / Tokio / Wasmtime, target workloads, and explicit non-goals.',
      sourceFile: 'docs/product.md',
      keywords: ['beam', 'tokio', 'wasmtime', 'positioning', 'memory', 'throughput', 'non-goals'],
    },
    {
      slug: 'architecture',
      order: 2,
      group: 'Positioning & Design',
      title: 'Architecture',
      shortTitle: 'Architecture',
      summary:
        '10-crate workspace dependency isolation, Cranelift AOT pipeline, managed C ABI boundary, and precise CFG root liveness.',
      sourceFile: 'docs/architecture.md',
      keywords: ['cranelift', 'aot', 'abi', 'ssa', 'cfg', 'crates', 'shadowframe', 'safepoint'],
    },
    {
      slug: 'language',
      order: 3,
      group: 'Language & Runtime',
      title: 'Language Design (wings-lang)',
      shortTitle: 'Language Design',
      summary:
        'actor / struct / state syntax, static immutable value types, non-blocking ask/tell, send_after timers, and monitor notifications.',
      sourceFile: 'docs/language.md',
      keywords: ['actor', 'struct', 'state', 'pid', 'ask', 'tell', 'send_after', 'monitor', 'wings-lsp'],
    },
    {
      slug: 'runtime',
      order: 4,
      group: 'Language & Runtime',
      title: 'Managed Microprocess Runtime',
      shortTitle: 'Runtime',
      summary:
        'O(1) slot+generation capability handles, pooled coroutine stacks, chunked precise GC, lock-free mailboxes, M:N preemption, and 3-tier admission.',
      sourceFile: 'docs/runtime.md',
      keywords: ['scheduler', 'gc', 'mailbox', 'stack pool', 'corosensei', 'tenant', 'quota', 'serve'],
    },
    {
      slug: 'evaluation',
      order: 5,
      group: 'Verification & Engineering',
      title: 'Evaluation & Acceptance',
      shortTitle: 'Evaluation',
      summary:
        'Paired BEAM reference workloads, current macOS AArch64 benchmark status, open tail-latency gates, and measurement discipline.',
      sourceFile: 'docs/evaluation.md',
      keywords: ['benchmark', 'p99', 'hot-queue', 'fairness', 'cgroup', 'rss', 'latency'],
    },
    {
      slug: 'roadmap',
      order: 6,
      group: 'Verification & Engineering',
      title: 'Risks & Roadmap',
      shortTitle: 'Risks & Roadmap',
      summary:
        '0.0.x zero-baggage evolution policy, status of technical slices 1–7, primary engineering risks, and M1–M5 milestones.',
      sourceFile: 'docs/roadmap.md',
      keywords: ['mvp', 'milestones', 'compatibility', '0.0.x', 'risks', 'supervision'],
    },
    {
      slug: 'publishing',
      order: 7,
      group: 'Verification & Engineering',
      title: 'Crate Publishing',
      shortTitle: 'Publishing',
      summary:
        'Apache-2.0 licensing, protected branch CI gates, topological crates.io release order across 10 crates, and release history.',
      sourceFile: 'docs/publishing.md',
      keywords: ['crates.io', 'cargo publish', 'release', '0.0.3', 'apache-2.0', 'msrv'],
    },
  ],
  zh: [
    {
      slug: 'product',
      order: 1,
      group: '定位与设计',
      title: '产品定位',
      shortTitle: '产品定位',
      summary: '性能与内存目标、核心基线对比（BEAM / Tokio / Wasmtime）、适用场景与非目标边界。',
      sourceFile: 'docs/zh/product.md',
      keywords: ['beam', 'tokio', 'wasmtime', '定位', '内存', '吞吐', '非目标'],
    },
    {
      slug: 'architecture',
      order: 2,
      group: '定位与设计',
      title: '架构设计',
      shortTitle: '架构设计',
      summary: '10 个 Workspace Crate 依赖隔离、Cranelift AOT 编译管线、受管 C ABI 与精确 CFG 引用根。',
      sourceFile: 'docs/zh/architecture.md',
      keywords: ['cranelift', 'aot', 'abi', '架构', '编译管线', 'shadowframe', 'safepoint'],
    },
    {
      slug: 'language',
      order: 3,
      group: '语言与运行时',
      title: '语言设计（wings-lang）',
      shortTitle: '语言设计',
      summary: 'actor / struct / state 语法、静态不可变值类型、异步 ask/tell、send_after 定时与 monitor 监控。',
      sourceFile: 'docs/zh/language.md',
      keywords: ['actor', 'struct', 'state', '语法', '类型', 'ask', 'send_after', 'monitor', 'lsp'],
    },
    {
      slug: 'runtime',
      order: 4,
      group: '语言与运行时',
      title: '受管微进程运行时',
      shortTitle: '运行时',
      summary: 'O(1) slot+generation 寻址、池化协程栈、进程内分块精确 GC、无锁快速邮箱、M:N 调度与三级账本。',
      sourceFile: 'docs/zh/runtime.md',
      keywords: ['运行时', '调度', 'gc', '邮箱', '协程栈', '配额', '账本', 'serve'],
    },
    {
      slug: 'evaluation',
      order: 5,
      group: '验收与工程',
      title: '评测与验收',
      shortTitle: '评测与验收',
      summary: '同语义 BEAM 对照负载、macOS AArch64 配对长测现状、门禁差距与优化实验纪律。',
      sourceFile: 'docs/zh/evaluation.md',
      keywords: ['评测', '基准', 'p99', '尾延迟', 'hot-queue', 'fairness', 'cgroup'],
    },
    {
      slug: 'roadmap',
      order: 6,
      group: '验收与工程',
      title: '风险与里程碑',
      shortTitle: '风险与里程碑',
      summary: '0.0.x 无包袱演进原则、MVP 七项技术切片进度、主要技术风险与后续里程碑规划。',
      sourceFile: 'docs/zh/roadmap.md',
      keywords: ['里程碑', '路线图', '风险', '兼容', 'mvp', '0.0.x'],
    },
    {
      slug: 'publishing',
      order: 7,
      group: '验收与工程',
      title: '包发布与版本记录',
      shortTitle: '包发布',
      summary: 'Apache-2.0 许可、分支保护门禁、10 个具名 Crate 拓扑发布顺序与 0.0.1–0.0.3 发布记录。',
      sourceFile: 'docs/zh/publishing.md',
      keywords: ['发布', 'crates.io', 'cargo', '0.0.3', '许可证', 'msrv'],
    },
  ],
};

export function getDocItems(locale: Locale): DocMeta[] {
  return DOC_ITEMS_BY_LOCALE[locale];
}

export function getDocMap(locale: Locale): Map<string, DocMeta> {
  return new Map(DOC_ITEMS_BY_LOCALE[locale].map((item) => [item.slug, item]));
}

export function withBase(path: string): string {
  const base = import.meta.env.BASE_URL || '/';
  const normalizedBase = base.endsWith('/') ? base.slice(0, -1) : base;
  const normalizedPath = path.startsWith('/') ? path : `/${path}`;
  return `${normalizedBase}${normalizedPath}`;
}

export function localePath(locale: Locale, subpath = '/'): string {
  const clean = subpath.startsWith('/') ? subpath : `/${subpath}`;
  if (locale === 'zh') {
    return withBase(clean === '/' ? '/zh/' : `/zh${clean}`);
  }
  return withBase(clean);
}

export function switchLocaleHref(currentPathname: string, targetLocale: Locale): string {
  const base = import.meta.env.BASE_URL || '/';
  const cleanBase = base.endsWith('/') ? base.slice(0, -1) : base;
  let route = currentPathname || '/';
  if (cleanBase && route.startsWith(cleanBase)) {
    route = route.slice(cleanBase.length) || '/';
  }
  if (!route.startsWith('/')) {
    route = `/${route}`;
  }
  if (route === '/zh' || route === '/zh/') {
    route = '/';
  } else if (route.startsWith('/zh/')) {
    route = route.slice(3);
  }
  if (!route.endsWith('/')) {
    route = `${route}/`;
  }
  return localePath(targetLocale, route);
}

export const UI_COPY = {
  en: {
    htmlLang: 'en',
    siteDescription:
      'Wings VM — A managed AOT microprocess virtual machine for high-concurrency stateful applications.',
    nav: {
      ariaMain: 'Main navigation',
      home: 'Overview',
      docs: 'Docs',
      language: 'Language',
      architecture: 'Architecture',
      evaluation: 'Benchmarks',
      searchPlaceholder: 'Jump to docs…',
      searchModalPlaceholder: 'Search documentation, runtime topics, CLI workflows…',
      searchEmpty: 'No matching chapters found.',
      searchHint: 'Navigate with ↑↓ · Enter to open · Esc to close',
      themeToggle: 'Toggle color theme',
      switchLabel: '中文',
      switchTitle: '切换到简体中文',
    },
    footer: {
      tagline:
        'Managed AOT microprocess virtual machine built with Rust & Cranelift. Apache-2.0 licensed; rapid 0.0.x iteration with zero legacy baggage.',
      colDocs: 'Documentation',
      colRuntime: 'Engineering',
      colLinks: 'Ecosystem',
      quickstart: 'Quick Start & CLI',
      language: 'wings-lang Guide',
      architecture: 'AOT Architecture',
      runtime: 'Runtime & GC',
      evaluation: 'BEAM Benchmarks',
      roadmap: 'Roadmap & MVP Gates',
      publishing: 'Crate Publishing',
      github: 'GitHub Repository',
      crates: 'crates.io Packages',
      altLangLabel: '简体中文站点',
    },
    docs: {
      titleSuffix: 'Wings VM Docs',
      sidebarAria: 'Documentation navigation',
      gettingStartedGroup: 'Getting Started',
      overviewLink: 'Quick Start & Overview',
      defaultGroup: 'Documentation',
      breadcrumbRoot: 'Docs',
      sourcePrefix: 'Edit / View',
      readInOtherLang: '中文版',
      prevChapter: 'Previous',
      backToOverview: 'Overview',
      nextChapter: 'Next',
      tocTitle: 'On This Page',
      tocAria: 'Table of contents',
      backToTop: 'Back to top',
      pagerAria: 'Chapter navigation',
      copyBtn: 'Copy',
      copiedBtn: 'Copied',
    },
  },
  zh: {
    htmlLang: 'zh-CN',
    siteDescription: 'Wings VM — 面向高并发有状态应用的受管 AOT 微进程虚拟机。',
    nav: {
      ariaMain: '主导航',
      home: '概览',
      docs: '文档',
      language: '语言',
      architecture: '架构',
      evaluation: '评测',
      searchPlaceholder: '搜索文档…',
      searchModalPlaceholder: '搜索章节、运行时机制、CLI 命令与基准测试…',
      searchEmpty: '未找到匹配的文档章节。',
      searchHint: '↑↓ 切换条目 · Enter 打开 · Esc 关闭',
      themeToggle: '切换深浅色主题',
      switchLabel: 'EN',
      switchTitle: 'Switch to English',
    },
    footer: {
      tagline:
        '基于 Rust 与 Cranelift 构建的受管 AOT 微进程虚拟机。遵循 Apache-2.0 许可，处于 0.0.x 无包袱快速演进阶段。',
      colDocs: '核心文档',
      colRuntime: '工程与验收',
      colLinks: '开源生态',
      quickstart: '快速开始与 CLI',
      language: 'wings-lang 语言指南',
      architecture: 'AOT 编译架构',
      runtime: '微进程运行时与 GC',
      evaluation: 'BEAM 对照评测',
      roadmap: '路线图与门禁',
      publishing: '包发布记录',
      github: 'GitHub 源码仓库',
      crates: 'crates.io 组件包',
      altLangLabel: 'English Site',
    },
    docs: {
      titleSuffix: 'Wings VM 文档',
      sidebarAria: '文档目录',
      gettingStartedGroup: '入门指南',
      overviewLink: '快速开始与总览',
      defaultGroup: '文档指南',
      breadcrumbRoot: '文档',
      sourcePrefix: '查看源码',
      readInOtherLang: 'English',
      prevChapter: '上一章',
      backToOverview: '返回总览',
      nextChapter: '下一章',
      tocTitle: '本页目录',
      tocAria: '本页小节',
      backToTop: '回到顶部',
      pagerAria: '上下章切换',
      copyBtn: '复制',
      copiedBtn: '已复制',
    },
  },
} as const;
