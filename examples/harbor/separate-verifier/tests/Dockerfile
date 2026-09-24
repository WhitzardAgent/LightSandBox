FROM python:3.12-slim
WORKDIR /tests
COPY test.sh /tests/test.sh
RUN chmod 755 /tests/test.sh && mkdir -p /logs/verifier
CMD ["sleep", "infinity"]
