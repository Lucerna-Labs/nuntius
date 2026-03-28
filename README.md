# Creator Command Center (CCC)
**A Next-Generation Media Hub by Lucerna Media**

Creator Command Center (CCC) is a revolutionary, high-performance, completely serverless-capable broadcasting and publishing platform written entirely in Rust. Originally inspired by traditional radio management tools, CCC has evolved into an ultra-efficient, multi-protocol hub for modern creators.

## Vision and Architecture

CCC is built from the ground up to minimize server strain, maximize concurrency, and provide unprecedented flexibility through a dynamic WebAssembly-ready plugin architecture.

### Key Innovations:

*   **P2P WebRTC Video & Audio:** Utilizing modern WebRTC mesh networks (via `p2p-media-loader`), CCC drastically reduces your server bandwidth. If 1,000 listeners tune into your live broadcast, they share the stream directly among themselves in the browser, reducing server load by up to 90%.
*   **Zero-Overhead Database (`redb`):** Instead of running a heavy SQL server, CCC uses `redb`—a pure Rust embedded key-value store. By leveraging clever zero-padded composite keys, CCC sorts massive datasets chronologically directly on disk, providing instant lookup times with zero CPU parsing overhead.
*   **Multi-Protocol Native Streaming:** Whether you need legacy Icecast (TCP) support for older hardware, or modern HTTP Live Streaming (HLS) for mobile apps, CCC's native Rust streaming engine handles it asynchronously with near-zero RAM footprint.
*   **GitHub-Backed Plugin Store:** Easily expand your station's capabilities (blogs, video podcasts, analytics) via our open-source, serverless plugin registry hosted directly on GitHub.
*   **API & Automation First:** Every feature is exposed via a high-performance REST API (`axum`) and a comprehensive CLI (`clap`), making CCC the perfect platform for automation by AI agents or custom scripts.

## Core Crates

*   `ccc-core`: The heart of the application, managing the `Plugin` trait ecosystem.
*   `ccc-database`: The ultra-fast, custom-indexed `redb` storage layer.
*   `ccc-radio-engine`: The native streaming engine supporting Icecast and HLS.
*   `ccc-api` & `ccc-cli`: The primary interfaces for interacting with the Command Center.
*   `ccc-plugin-*`: Extensible features including Video Podcasting, Blogging, and the decentralized Store Registry.

---

## License

Creator Command Center (CCC) is released under the **PolyForm Small Business License 1.0.0** (Fair Code).

It is **100% free and open-source** for individuals, non-profits, and small businesses.
However, it is **restricted for use by Large Enterprises** (organizations with more than 100 employees or >$5,000,000 USD annual gross revenue).

If you represent a Large Enterprise and wish to use CCC in production, you must contact **Lucerna Media** to purchase a commercial license. Please see the `LICENSE` file for full details.
