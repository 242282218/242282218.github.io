import { getCollection } from 'astro:content';
import type { APIRoute } from 'astro';
import { publishedPosts } from '../lib/posts';

function escapeXml(value: string): string {
  return value.replace(/[&<>"']/g, (char) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&apos;',
  })[char] ?? char);
}

export const GET: APIRoute = async ({ site }) => {
  if (!site) throw new Error('Missing site URL for RSS feed');
  const posts = publishedPosts(await getCollection('blog'));
  const items = posts.map((post) => `<item>
    <title>${escapeXml(post.data.title)}</title>
    <link>${new URL(`/blog/${post.id}/`, site).href}</link>
    <guid>${new URL(`/blog/${post.id}/`, site).href}</guid>
    <pubDate>${post.data.pubDate.toUTCString()}</pubDate>
    <description>${escapeXml(post.data.description)}</description>
  </item>`).join('\n');

  const feed = `<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel>
<title>观澜志</title>
<link>${site.href}</link>
<description>项目实践、技术文章与学习笔记。</description>
<language>zh-CN</language>
${items}
</channel></rss>`;
  return new Response(feed, { headers: { 'Content-Type': 'application/rss+xml; charset=utf-8' } });
};
