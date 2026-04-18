# shopflow

A microservices platform for an online shop. The repository is
partitioned by **business domain** — each top-level directory owns
a bounded context with its own data model, service, and API:

- `users/` — account registration, profile, authentication.
- `orders/` — checkout, order lifecycle, payment integration.
- `inventory/` — stock levels, warehouse sync, reservation.

Each domain is independently deployable.
