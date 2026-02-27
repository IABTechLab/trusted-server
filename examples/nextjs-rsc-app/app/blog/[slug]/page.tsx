// Dynamic blog page — produces larger RSC payloads that may span multiple
// script tags (cross-script T-chunks). The large content block forces
// Next.js to split the Flight data across multiple inlined scripts.
const ORIGIN = "https://origin.example.com:3099";

// Generate enough content to produce cross-script T-chunks
function generateArticleContent(): string[] {
  const paragraphs: string[] = [];
  for (let i = 0; i < 20; i++) {
    paragraphs.push(
      `Paragraph ${i + 1}: This content references ${ORIGIN}/article/${i + 1} ` +
      `and includes links to ${ORIGIN}/category/tech and ${ORIGIN}/author/staff. ` +
      `For more information, visit ${ORIGIN}/resources/guide-${i + 1}.`
    );
  }
  return paragraphs;
}

interface PageProps {
  params: Promise<{ slug: string }>;
}

export default async function BlogPost({ params }: PageProps) {
  const { slug } = await params;
  const paragraphs = generateArticleContent();

  return (
    <article>
      <h1>Blog Post: {slug}</h1>
      <div>
        <span>Published on </span>
        <a href={`${ORIGIN}/blog`}>the blog</a>
      </div>

      <div>
        <img
          src={`${ORIGIN}/images/blog/${slug}/hero.jpg`}
          alt={`Hero image for ${slug}`}
          width={1200}
          height={630}
        />
      </div>

      {paragraphs.map((text, i) => (
        <p key={i}>{text}</p>
      ))}

      <nav>
        <h2>Related Posts</h2>
        <ul>
          {Array.from({ length: 5 }, (_, i) => (
            <li key={i}>
              <a href={`${ORIGIN}/blog/related-post-${i + 1}`}>
                Related Post {i + 1}
              </a>
            </li>
          ))}
        </ul>
      </nav>

      <footer>
        <a href={`${ORIGIN}/blog/${slug}/comments`}>View Comments</a>
        <a href={`${ORIGIN}/blog/${slug}/share`}>Share</a>
      </footer>
    </article>
  );
}

// Pre-render a known slug for fixture capture
export function generateStaticParams() {
  return [{ slug: "hello-world" }];
}
