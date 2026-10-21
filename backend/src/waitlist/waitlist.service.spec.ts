import { Test } from '@nestjs/testing';
import { getRepositoryToken } from '@nestjs/typeorm';
import { Between, LessThanOrEqual, MoreThanOrEqual } from 'typeorm';
import { WaitlistService } from './waitlist.service';
import { WaitlistEntry } from './entities/waitlist.entity';

describe('WaitlistService', () => {
  let service: WaitlistService;
  const find = jest.fn();

  beforeEach(async () => {
    find.mockReset();
    find.mockResolvedValue([]);

    const moduleRef = await Test.createTestingModule({
      providers: [
        WaitlistService,
        {
          provide: getRepositoryToken(WaitlistEntry),
          useValue: { find },
        },
      ],
    }).compile();

    service = moduleRef.get(WaitlistService);
  });

  it('applies Between date constraints in the find where clause', async () => {
    const from = '2024-01-01T00:00:00.000Z';
    const to = '2024-02-01T00:00:00.000Z';

    await service.exportCsv({ dateFrom: from, dateTo: to });

    expect(find).toHaveBeenCalledWith({
      where: { createdAt: Between(new Date(from), new Date(to)) },
      order: { createdAt: 'ASC' },
    });
  });

  it('applies MoreThanOrEqual when only dateFrom is provided', async () => {
    const from = '2024-01-01T00:00:00.000Z';

    await service.exportCsv({ dateFrom: from });

    expect(find).toHaveBeenCalledWith({
      where: { createdAt: MoreThanOrEqual(new Date(from)) },
      order: { createdAt: 'ASC' },
    });
  });

  it('applies LessThanOrEqual when only dateTo is provided', async () => {
    const to = '2024-02-01T00:00:00.000Z';

    await service.exportCsv({ dateTo: to });

    expect(find).toHaveBeenCalledWith({
      where: { createdAt: LessThanOrEqual(new Date(to)) },
      order: { createdAt: 'ASC' },
    });
  });

  it('queries without date constraints when no range is provided', async () => {
    await service.exportCsv();

    expect(find).toHaveBeenCalledWith({
      where: {},
      order: { createdAt: 'ASC' },
    });
  });
});
