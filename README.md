# OmaTuber

Na raiz do projeto:

```sh
make
omatuber
```

O `make` compila e instala o aplicativo e o plugin do OBS para o usuário, com
DMA-BUF ativado no launcher. Reinicie o OBS depois da instalação.
`make build` apenas compila; `make test` executa os testes.

Requisitos e limitações do backend experimental: [DMA-BUF](docs/dmabuf.md).
