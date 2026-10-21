import {
  Column,
  CreateDateColumn,
  Entity,
  PrimaryGeneratedColumn,
  UpdateDateColumn,
} from 'typeorm';

@Entity('merchants')
export class Merchant {
  @PrimaryGeneratedColumn('uuid')
  id: string;

  @Column({ unique: true })
  email: string;

  @Column()
  name: string;

  @Column({ nullable: true })
  passwordHash?: string;

  @Column({ nullable: true, select: false })
  apiKeyHash?: string;

  @Column({ nullable: true, select: false })
  apiKeyLookupHash?: string;

  @Column({ nullable: true })
  apiKeyLastFour?: string;

  @Column({ type: 'timestamptz', nullable: true })
  apiKeyCreatedAt?: Date;

  @Column({ type: 'timestamptz', nullable: true })
  apiKeyRevokedAt?: Date;

  @CreateDateColumn()
  createdAt: Date;

  @UpdateDateColumn()
  updatedAt: Date;
}
