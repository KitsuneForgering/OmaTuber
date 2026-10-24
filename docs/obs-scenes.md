# Configurar a coleção de cenas do OBS

Depois de `make`, reabra o OmaTuber e conecte-o ao OBS. No cartão de estado da
cena, use **Configurar todas as cenas**. O aplicativo informa quantas cenas da
**coleção atual** foram configuradas e lista pendências por cena/grupo.
A operação não troca de coleção, não inicia gravação e não inicia transmissão.

A configuração usa uma única fonte compartilhada do backend ativo. No modo
DMA-BUF, também reconhece fontes criadas manualmente, mesmo sem URL e depois
de renomeadas. Fontes browser antigas são reconhecidas pela URL exata do
OmaTuber; fontes de câmera, áudio e outros sites não entram na migração.

- Cenas sem avatar recebem um vínculo para a fonte compartilhada.
- Uma cena que já herda o avatar de um grupo ou cena aninhada não recebe outra
  cópia por cima. Grupos compartilhados são processados uma vez.
- A substituição copia posição, escala, rotação, crop, bounds, bloqueio, modo
  de mistura e ordem do item anterior. A visibilidade é preservada.
- Itens antigos e instâncias redundantes ficam ocultos. Seus inputs e suas
  configurações permanecem no OBS para recuperação; não são apagados.
- O plugin suspende conexões de fontes que não estão sendo exibidas, para que
  uma instância antiga oculta não ocupe o produtor DMA-BUF.
- Rodar novamente reutiliza os vínculos existentes. Não cria um novo input
  por cena nem altera a configuração em cada conexão automaticamente.

Se havia várias instâncias diferentes do avatar no mesmo container, a
configuração consolida a exibição numa só; as demais permanecem ocultas.
Filtros associados aos inputs antigos permanecem nesses inputs: não são
combinados nem copiados para a fonte compartilhada. A fonte compartilhada
mantém os seus próprios filtros.

O protocolo não fornece transação para todas as cenas. O aplicativo prepara a
substituição antes de ocultar a original e tenta restaurar os estados quando
uma etapa falha; outras cenas já concluídas permanecem configuradas. A
interface informa o progresso parcial. É possível executar novamente para
concluir. Desconectar cancela a tarefa e interrompe o envio de novas alterações; uma
requisição já enviada ao OBS pode ter sido aplicada.

Limitação: o cliente obws atual lê `cropToBounds`, mas não oferece esse campo
na escrita da transformação. Se a substituição exigir copiar um item com essa
opção ativada, ela fica pendente, com a fonte original preservada, em vez de
perder silenciosamente o recorte. Esse caso exige ajuste manual.

Os testes usam OBS WebSocket simulado: migração entre backends, input único,
reexecução, instâncias manuais, duplicatas, grupos e cenas aninhadas, layout,
visibilidade, falha de transformação, falha na atualização inicial e plugin
ausente. A interface e o worker têm testes de acionamento e conclusão.

Referência do protocolo usado:
[obs-websocket](https://github.com/obsproject/obs-websocket/blob/master/docs/generated/protocol.md).
