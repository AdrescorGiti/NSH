FROM registry.fedoraproject.org/fedora@sha256:b174cd5906c712f59a675cc85ea2ca8ff3bcf3643f1a1f3bc4e14dc28bcf1872
RUN dnf -y install rpm-build tar && dnf clean all
RUN mkdir -p /work && chown 1000:1000 /work
USER 1000:1000
WORKDIR /work
