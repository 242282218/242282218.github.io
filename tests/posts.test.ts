import assert from 'node:assert/strict';
import test from 'node:test';
import type { CollectionEntry } from 'astro:content';
import { publishedPosts } from '../src/lib/posts.ts';

type Post = CollectionEntry<'blog'>;

function post(id: string, pubDate: string, draft = false): Post {
  return {
    id,
    collection: 'blog',
    data: { title: id, description: id, pubDate: new Date(pubDate), draft },
  } as Post;
}

test('publishes non-draft entries in reverse chronological order', () => {
  const entries = [post('old', '2023-01-01'), post('draft', '2025-01-01', true), post('new', '2024-01-01')];
  assert.deepEqual(publishedPosts(entries).map(({ id }) => id), ['new', 'old']);
});

test('an empty collection has no published entries', () => {
  assert.deepEqual(publishedPosts([]), []);
});

test('sorting does not mutate content collection order', () => {
  const entries = [post('old', '2023-01-01'), post('new', '2024-01-01')];
  publishedPosts(entries);
  assert.deepEqual(entries.map(({ id }) => id), ['old', 'new']);
});
