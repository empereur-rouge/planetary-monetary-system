FROM ubuntu:latest
LABEL authors="erwan.ngma"

ENTRYPOINT ["top", "-b"]