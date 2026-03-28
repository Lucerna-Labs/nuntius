# AzuraCast Rust Rewrite

A high-performance, RAM and CPU efficient rewrite of the AzuraCast official app in Rust.

## Vision and Goals

- **Performance**: Convert as much of AzuraCast into Rust as possible. The primary goal is to make the software incredibly efficient in terms of both RAM and CPU usage.
- **Plugin Architecture**: Implement a robust plugin extension system. Plugins are built as Rust crates that hook into the core application to expand functionality, such as adding a static blog or a newsletter.
- **Automation First**: The application is fully REST API capable and includes a comprehensive Command Line Interface (CLI). This ensures the app can be seamlessly automated and managed by LLM agents.
- **Customizable UI**: Expand UI theme customizations to give users complete control over the look and feel of their radio station management panel.

## Project Structure

This project is organized as a Cargo workspace containing the following crates:

- `core`: Contains the core application logic and defines the traits required for the plugin system.
- `api`: A foundational REST API (built with `axum`) to facilitate external communication and agent automation.
- `cli`: A command-line interface (built with `clap`) for managing the server directly from the terminal.
- `plugin-blog`: An example plugin demonstrating how to extend the core functionality.

## Getting Started

*(Instructions for building and running will be added here as development progresses)*
