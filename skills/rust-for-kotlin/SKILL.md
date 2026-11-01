---
name: rust-for-kotlin
description: Explain Rust code or concepts to a Kotlin developer, including ownership, borrowing, traits, and async execution.
---

# Rust for a Kotlin developer

1. Read the relevant Rust code and identify the concept the user is asking about.
2. Explain its runtime behavior and Rust's compile-time guarantees, using the actual types and operations in that code.
3. Give the closest Kotlin analogy, then state where the analogy breaks. For example, `Arc::clone` shares an allocation rather than copying its contents; a Kotlin reference also shares an object, but JVM garbage collection and Rust reference counting have different lifetimes and costs.
4. Finish with a short **Key difference** summary tied to the example.

Keep explanations focused on the requested concept. Treat explanation-only requests as read-only; change files when the user asks for a change.
