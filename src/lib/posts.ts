import type { CollectionEntry } from 'astro:content';

export function publishedPosts(posts: CollectionEntry<'blog'>[]): CollectionEntry<'blog'>[] {
  return posts
    .filter(({ data }) => !data.draft)
    .sort((a, b) => b.data.pubDate.getTime() - a.data.pubDate.getTime());
}
